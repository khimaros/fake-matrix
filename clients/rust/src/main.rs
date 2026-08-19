// matrix-sdk's deeply-nested async futures overflow the default type-recursion limit.
#![recursion_limit = "256"]
//! mxhelper: the human counterparty for encrypted-room e2e tests. it drives a REAL matrix-sdk
//! client (e2e-encryption compiled in) against the fake homeserver, which relays the crypto
//! artifacts (device keys, one-time keys, to-device messages, room events) between it and the bot.
//!
//! flow: log in, sync to settle state + upload keys + discover the bot's device, send one ENCRYPTED
//! message into the room, then sync until the bot's ENCRYPTED reply arrives and print it decrypted.
//! stdout is the contract: `SENT` after the send, then `REPLY: <plaintext>` on the bot's answer.
//!
//! config is via env: HELPER_HOMESERVER, HELPER_USER (localpart), HELPER_PASSWORD (any, the fake
//! accepts all), HELPER_ROOM (an encrypted room id), HELPER_SEND (the message text).

use std::time::Duration;

use matrix_sdk::config::SyncSettings;
use matrix_sdk::ruma::events::room::message::{
    MessageType, OriginalSyncRoomMessageEvent, RoomMessageEventContent,
};
use matrix_sdk::ruma::RoomId;
use matrix_sdk::{Client, Room};
use tokio::sync::mpsc;

fn env(key: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| panic!("mxhelper: missing env {key}"))
}

// how long to wait for the bot's decrypted reply before giving up (the bot drives an llm turn).
const REPLY_TIMEOUT: Duration = Duration::from_secs(90);

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let homeserver = env("HELPER_HOMESERVER");
    let user = env("HELPER_USER");
    let password = std::env::var("HELPER_PASSWORD").unwrap_or_else(|_| "secret".into());
    let room_id = env("HELPER_ROOM");
    let send_text = env("HELPER_SEND");

    let store = std::env::temp_dir().join(format!("mxhelper-{}", std::process::id()));
    std::fs::create_dir_all(&store).expect("mxhelper: create store dir");

    let client = Client::builder()
        .homeserver_url(&homeserver)
        .sqlite_store(&store, None)
        .build()
        .await
        .expect("mxhelper: build client");

    client
        .matrix_auth()
        .login_username(&user, &password)
        .initial_device_display_name("mxhelper")
        .send()
        .await
        .expect("mxhelper: login");
    let me = client
        .user_id()
        .expect("mxhelper: logged-in user id")
        .to_owned();
    eprintln!("mxhelper: logged in as {me}");

    // capture the bot's (non-self) decrypted text replies onto a channel.
    let (tx, mut rx) = mpsc::channel::<String>(16);
    client.add_event_handler(move |ev: OriginalSyncRoomMessageEvent, _room: Room| {
        let tx = tx.clone();
        let me = me.clone();
        async move {
            if ev.sender == me {
                return;
            }
            if let MessageType::Text(t) = ev.content.msgtype {
                let _ = tx.send(t.body).await;
            }
        }
    });

    // settle state before sending: the first sync uploads our keys + learns the room, and a second
    // lets the device-list / key-query round-trip complete so the outbound megolm session can be
    // shared with the bot's device.
    let settings = SyncSettings::default().timeout(Duration::from_secs(5));
    for _ in 0..2 {
        client
            .sync_once(settings.clone())
            .await
            .expect("mxhelper: initial sync");
    }

    let rid = RoomId::parse(&room_id).expect("mxhelper: bad room id");
    let room = client
        .get_room(&rid)
        .expect("mxhelper: room not found after sync");
    room.send(RoomMessageEventContent::text_plain(&send_text))
        .await
        .expect("mxhelper: send encrypted message");
    println!("SENT");

    // keep syncing in the background while we wait for the bot's encrypted reply.
    let sync_client = client.clone();
    tokio::spawn(async move {
        let _ = sync_client.sync(SyncSettings::default()).await;
    });

    match tokio::time::timeout(REPLY_TIMEOUT, rx.recv()).await {
        Ok(Some(body)) => println!("REPLY: {body}"),
        _ => {
            eprintln!("mxhelper: no reply within timeout");
            std::process::exit(2);
        }
    }
}
