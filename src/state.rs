// shared homeserver state: identities, rooms (some encrypted), a unified append-only delivery
// log keyed by a monotonic sequence (the /sync token), and the bot's captured outbound sends.
// cloned into each request thread as an Arc.
//
// the fake performs NO cryptography. for end-to-end encrypted rooms it acts as a pure RELAY
// between two real matrix-sdk clients (the bot and a helper): it stores and forwards the key
// material and to-device messages the SDKs produce (/keys/upload, /keys/query, /keys/claim,
// /sendToDevice) and delivers each room's events to its OTHER members. the crypto (olm/megolm)
// stays entirely inside the clients. unencrypted rooms keep the simpler inject + capture path.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

use crate::config::Config;

// the bot's whoami device id; matrix-sdk aborts if whoami returns none (e2e needs one).
pub const BOT_DEVICE_ID: &str = "FAKEDEVICE";
// the server name in the mxc uri of injected media.
const MEDIA_SERVER: &str = "fake.local";
pub const SUPPORTED_VERSIONS: &[&str] = &[
    "v1.1", "v1.2", "v1.3", "v1.4", "v1.5", "v1.6", "v1.7", "v1.8", "v1.9", "v1.10", "v1.11",
];
// how long an incremental /sync holds before returning empty (a real server's timeout).
const SYNC_HOLD: Duration = Duration::from_secs(25);

pub type Shared = Arc<State>;

pub struct Room {
    pub id: String,
    pub members: Vec<String>,
    pub is_direct: bool,
    pub creator: String,
    pub encrypted: bool,
}

// a device's uploaded key material, relayed verbatim between clients.
#[derive(Default)]
struct DeviceKeys {
    identity: Option<Value>, // the signed device_keys object from /keys/upload
    one_time: HashMap<String, Value>, // key_id -> key object, popped by /keys/claim
}

// one deliverable in the unified log: a room event (to every member but the sender) or a
// to-device message (to one device). the /sync cursor is the global seq over both.
enum Delivery {
    Room {
        room_id: String,
        sender: String,
        event: Value,
    },
    ToDevice {
        user: String,
        device: String,
        event: Value,
    },
}

struct Log {
    seq: u64,
    entries: Vec<(u64, Delivery)>,
    key_changes: Vec<(u64, String)>, // (seq, user) each time a device (re)uploads keys
}

pub struct State {
    pub bot_user: String,
    pub bot_display_name: String,
    pub rooms: Vec<Room>,
    tokens: Mutex<HashMap<String, (String, String)>>, // access_token -> (user, device)
    devices: Mutex<HashMap<(String, String), DeviceKeys>>,
    log: Mutex<Log>,
    log_cv: Condvar,
    captures: Mutex<Vec<Value>>,
    // read receipts posted by clients. a bot marks a message read once it has PROCESSED it, even
    // when no reply goes out, so this capture is the only observable proof of that half.
    receipts: Mutex<Vec<Value>>,
    logins: Mutex<u64>, // monotonic counter for generated device ids / tokens
    // syncs served per user. a test that injects before its client's FIRST sync loses the event
    // forever: the initial sync returns current state (not backlog) and hands back a token PAST
    // the injection, so every later poll asks for events newer than it. waiting on this counter
    // is how a test knows the client has a token and is listening.
    syncs: Mutex<HashMap<String, u64>>,
    // media a test injected with a message: id -> (mimetype, bytes), served by the download routes.
    media: Mutex<HashMap<String, (String, Vec<u8>)>>,
}

// what an injected message carries when it is a file and not text.
pub struct Media<'a> {
    pub msgtype: &'a str,
    pub filename: &'a str,
    pub mimetype: &'a str,
    pub bytes: Vec<u8>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// an opaque event-id fragment: matrix event ids must not carry a ':' (ruma reads the tail as a
// server name), so strip every sigil to an alphanumeric slug that is still stable per id.
fn id_frag(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect()
}

impl State {
    pub fn from_config(c: &Config) -> State {
        let rooms = vec![
            Room {
                id: c.dm_room.clone(),
                members: vec![c.bot_user.clone(), c.human.clone()],
                is_direct: true,
                creator: c.bot_user.clone(),
                encrypted: false,
            },
            Room {
                id: c.group_room.clone(),
                members: vec![c.bot_user.clone(), c.human.clone(), c.other.clone()],
                is_direct: false,
                creator: c.bot_user.clone(),
                encrypted: false,
            },
            Room {
                id: c.enc_room.clone(),
                members: vec![c.bot_user.clone(), c.human.clone()],
                is_direct: true,
                creator: c.bot_user.clone(),
                encrypted: true,
            },
            Room {
                id: c.quiet_room.clone(),
                members: vec![c.bot_user.clone(), c.other.clone()],
                is_direct: true,
                creator: c.bot_user.clone(),
                encrypted: false,
            },
        ];
        State {
            bot_user: c.bot_user.clone(),
            bot_display_name: c.bot_display_name.clone(),
            rooms,
            tokens: Mutex::new(HashMap::new()),
            devices: Mutex::new(HashMap::new()),
            log: Mutex::new(Log {
                seq: 0,
                entries: Vec::new(),
                key_changes: Vec::new(),
            }),
            log_cv: Condvar::new(),
            captures: Mutex::new(Vec::new()),
            receipts: Mutex::new(Vec::new()),
            logins: Mutex::new(0),
            syncs: Mutex::new(HashMap::new()),
            media: Mutex::new(HashMap::new()),
        }
    }

    // resolve a bearer token to (user, device). an unregistered token is the restore-session bot
    // (it never logs in), so it maps to the bot identity; /login tokens map to their helper device.
    pub fn resolve(&self, token: &str) -> (String, String) {
        self.tokens
            .lock()
            .unwrap()
            .get(token)
            .cloned()
            .unwrap_or_else(|| (self.bot_user.clone(), BOT_DEVICE_ID.to_string()))
    }

    // register a fresh device for a /login and return (user_id, device_id, access_token).
    pub fn login(&self, user: &str) -> (String, String, String) {
        let user = if user.starts_with('@') {
            user.to_string()
        } else {
            let domain = self
                .bot_user
                .split_once(':')
                .map(|(_, d)| d)
                .unwrap_or("fake.local");
            format!("@{user}:{domain}")
        };
        let mut n = self.logins.lock().unwrap();
        *n += 1;
        let device = format!("HELPER{n}");
        let token = format!("helper_token_{n}");
        self.tokens
            .lock()
            .unwrap()
            .insert(token.clone(), (user.clone(), device.clone()));
        (user, device, token)
    }

    // store a device's identity + one-time keys, mark its user's device list changed, and return
    // the current one-time-key counts (matrix-sdk tops up when the count drops).
    pub fn upload_keys(&self, token: &str, device_keys: &Value, otks: &Value) -> Value {
        let (user, device) = self.resolve(token);
        let mut devs = self.devices.lock().unwrap();
        let entry = devs.entry((user.clone(), device.clone())).or_default();
        if device_keys.is_object() {
            entry.identity = Some(device_keys.clone());
        }
        if let Some(map) = otks.as_object() {
            for (k, v) in map {
                entry.one_time.insert(k.clone(), v.clone());
            }
        }
        let count = entry.one_time.len() as u64;
        drop(devs);
        self.mark_keys_changed(&user);
        json!({ "one_time_key_counts": { "signed_curve25519": count } })
    }

    fn mark_keys_changed(&self, user: &str) {
        let mut log = self.log.lock().unwrap();
        log.seq += 1;
        let seq = log.seq;
        log.key_changes.push((seq, user.to_string()));
        self.log_cv.notify_all();
    }

    // /keys/query: every requested user's devices, each with its uploaded identity keys.
    pub fn query_keys(&self, users: &[String]) -> Value {
        let devs = self.devices.lock().unwrap();
        let mut out = serde_json::Map::new();
        for user in users {
            let mut per_device = serde_json::Map::new();
            for ((u, d), keys) in devs.iter() {
                if u == user {
                    if let Some(identity) = &keys.identity {
                        per_device.insert(d.clone(), identity.clone());
                    }
                }
            }
            out.insert(user.clone(), Value::Object(per_device));
        }
        json!({ "device_keys": out, "failures": {} })
    }

    // /keys/claim: pop one one-time key per requested (user, device).
    pub fn claim_keys(&self, requests: &Value) -> Value {
        let mut devs = self.devices.lock().unwrap();
        let mut out = serde_json::Map::new();
        if let Some(by_user) = requests.as_object() {
            for (user, by_device) in by_user {
                let mut per_device = serde_json::Map::new();
                if let Some(dmap) = by_device.as_object() {
                    for (device, _algo) in dmap {
                        if let Some(keys) = devs.get_mut(&(user.clone(), device.clone())) {
                            if let Some(key_id) = keys.one_time.keys().next().cloned() {
                                let key = keys.one_time.remove(&key_id).unwrap();
                                per_device.insert(device.clone(), json!({ key_id: key }));
                            }
                        }
                    }
                }
                out.insert(user.clone(), Value::Object(per_device));
            }
        }
        json!({ "one_time_keys": out, "failures": {} })
    }

    // /sendToDevice: fan a to-device payload out to each addressed (user, device) queue. a "*"
    // device targets every known device of that user.
    pub fn send_to_device(&self, token: &str, ev_type: &str, messages: &Value) {
        let (sender, _) = self.resolve(token);
        let known: Vec<(String, String)> = self.devices.lock().unwrap().keys().cloned().collect();
        let mut log = self.log.lock().unwrap();
        if let Some(by_user) = messages.as_object() {
            for (user, by_device) in by_user {
                if let Some(dmap) = by_device.as_object() {
                    for (device, content) in dmap {
                        let targets: Vec<String> = if device == "*" {
                            known
                                .iter()
                                .filter(|(u, _)| u == user)
                                .map(|(_, d)| d.clone())
                                .collect()
                        } else {
                            vec![device.clone()]
                        };
                        for d in targets {
                            log.seq += 1;
                            let seq = log.seq;
                            let event =
                                json!({ "type": ev_type, "sender": sender, "content": content });
                            log.entries.push((
                                seq,
                                Delivery::ToDevice {
                                    user: user.clone(),
                                    device: d,
                                    event,
                                },
                            ));
                        }
                    }
                }
            }
        }
        self.log_cv.notify_all();
    }

    // a client PUT /rooms/{room}/send/{type}/{txn}: capture it (for assertions) and deliver it to
    // the room's OTHER members. used by both the bot's replies and a helper's encrypted sends.
    pub fn room_send(&self, token: &str, room_id: &str, ev_type: &str, content: &Value) -> String {
        let (sender, _) = self.resolve(token);
        let n = {
            let mut caps = self.captures.lock().unwrap();
            caps.push(json!({
                "room_id": room_id, "type": ev_type, "sender": sender,
                "msgtype": content.get("msgtype"), "body": content.get("body"), "content": content,
            }));
            caps.len()
        };
        let event_id = format!("$sent{n}");
        let mut log = self.log.lock().unwrap();
        log.seq += 1;
        let seq = log.seq;
        let event = json!({
            "type": ev_type, "sender": sender, "event_id": event_id,
            "origin_server_ts": now_ms(), "content": content, "room_id": room_id,
        });
        log.entries.push((
            seq,
            Delivery::Room {
                room_id: room_id.to_string(),
                sender,
                event,
            },
        ));
        self.log_cv.notify_all();
        event_id
    }

    // queue an inbound plaintext m.text into an UNENCRYPTED room (the admin inject path; the human
    // has no real client there). encrypted rooms are driven by a real helper via room_send instead.
    pub fn inject(
        &self,
        room_id: &str,
        sender: &str,
        body: &str,
        formatted: Option<&str>,
        mentions: Option<&Value>,
        media: Option<Media<'_>>,
    ) -> String {
        let mut content = json!({"msgtype": "m.text", "body": body});
        // a media message: `body` is its caption when it differs from the filename (matrix v1.10).
        if let Some(m) = media {
            let mut store = self.media.lock().unwrap();
            let id = format!("media{}", store.len() + 1);
            content["msgtype"] = json!(m.msgtype);
            content["filename"] = json!(m.filename);
            content["url"] = json!(format!("mxc://{MEDIA_SERVER}/{id}"));
            content["info"] = json!({"mimetype": m.mimetype, "size": m.bytes.len()});
            store.insert(id, (m.mimetype.to_string(), m.bytes));
        }
        if let Some(html) = formatted {
            content["format"] = json!("org.matrix.custom.html");
            content["formatted_body"] = json!(html);
        }
        if let Some(m) = mentions {
            content["m.mentions"] = m.clone();
        }
        let mut log = self.log.lock().unwrap();
        log.seq += 1;
        let seq = log.seq;
        // returned to the caller: a test asserting that a REPLY targets this message needs the id
        // the event was minted with, and deriving it from the seq would duplicate this format.
        let event_id = format!("$evt{seq}");
        let event = json!({
            "type": "m.room.message", "sender": sender, "event_id": event_id,
            "origin_server_ts": now_ms(), "content": content,
            "room_id": room_id,
        });
        log.entries.push((
            seq,
            Delivery::Room {
                room_id: room_id.to_string(),
                sender: sender.to_string(),
                event,
            },
        ));
        self.log_cv.notify_all();
        crate::log(format_args!(
            "inject seq={seq} room={room_id} sender={sender} ({} bytes)",
            body.len()
        ));
        event_id
    }

    pub fn media(&self, id: &str) -> Option<(String, Vec<u8>)> {
        self.media.lock().unwrap().get(id).cloned()
    }

    pub fn sent_snapshot(&self) -> Vec<Value> {
        self.captures.lock().unwrap().clone()
    }

    // record one read receipt. the real endpoint answers an empty 200 and the marker shows up only
    // in another user's sync, so a test asserts on this capture instead.
    pub fn receipt(&self, token: &str, room_id: &str, receipt_type: &str, event_id: &str) {
        let (user, _) = self.resolve(token);
        crate::log(format_args!(
            "receipt room={room_id} user={user} type={receipt_type} event={event_id}"
        ));
        self.receipts.lock().unwrap().push(json!({
            "room_id": room_id, "user_id": user,
            "type": receipt_type, "event_id": event_id,
        }));
    }

    pub fn receipts_snapshot(&self) -> Vec<Value> {
        self.receipts.lock().unwrap().clone()
    }

    // clear the delivery log + captures but keep seq monotonic so outstanding cursors stay valid.
    pub fn reset(&self) {
        let mut log = self.log.lock().unwrap();
        log.entries.clear();
        log.key_changes.clear();
        self.captures.lock().unwrap().clear();
        self.receipts.lock().unwrap().clear();
        self.log_cv.notify_all();
    }

    pub fn sync(&self, token: &str, since: Option<&str>) -> Value {
        let (user, device) = self.resolve(token);
        *self.syncs.lock().unwrap().entry(user.clone()).or_insert(0) += 1;
        match since {
            None | Some("") => self.initial_sync(&user),
            Some(s) => self.incremental_sync(&user, &device, s),
        }
    }

    /// syncs served per user, for a test waiting until a client is actually listening.
    pub fn sync_counts(&self) -> HashMap<String, u64> {
        self.syncs.lock().unwrap().clone()
    }

    // initial sync: every room the user is in appears joined with its create + member (+ encryption)
    // state; the user's encrypted-room peers seed device_lists.changed so the SDK fetches their keys.
    fn initial_sync(&self, user: &str) -> Value {
        let seq = self.log.lock().unwrap().seq;
        let mut joined = serde_json::Map::new();
        let mut peers: Vec<String> = Vec::new();
        for room in self
            .rooms
            .iter()
            .filter(|r| r.members.iter().any(|m| m == user))
        {
            joined.insert(
                room.id.clone(),
                json!({
                    "state": {"events": self.room_state(room)},
                    "timeline": {"events": [], "limited": false, "prev_batch": "s0"},
                }),
            );
            if room.encrypted {
                peers.extend(room.members.iter().filter(|m| *m != user).cloned());
            }
        }
        peers.sort();
        peers.dedup();
        let direct = self.direct_for(user);
        let account = if direct.is_empty() {
            vec![]
        } else {
            vec![json!({"type": "m.direct", "content": direct})]
        };
        json!({
            "next_batch": format!("s{seq}"),
            "rooms": {"join": joined, "invite": {}, "leave": {}},
            "account_data": {"events": account},
            "device_lists": {"changed": peers, "left": []},
            "to_device": {"events": []},
        })
    }

    // incremental sync: long-poll until the log advances past the cursor, then return this device's
    // fresh room events (from other senders), to-device messages, and changed device lists.
    fn incremental_sync(&self, user: &str, device: &str, since: &str) -> Value {
        let cursor: u64 = since.trim_start_matches('s').parse().unwrap_or(0);
        let log = self.log.lock().unwrap();
        let (log, _timed_out) = self
            .log_cv
            .wait_timeout_while(log, SYNC_HOLD, |l| l.seq <= cursor)
            .unwrap();
        let member_of: Vec<&str> = self
            .rooms
            .iter()
            .filter(|r| r.members.iter().any(|m| m == user))
            .map(|r| r.id.as_str())
            .collect();
        let mut by_room: HashMap<String, Vec<Value>> = HashMap::new();
        let mut to_device: Vec<Value> = Vec::new();
        for (s, entry) in log.entries.iter() {
            if *s <= cursor {
                continue;
            }
            match entry {
                Delivery::Room {
                    room_id,
                    sender,
                    event,
                } => {
                    // both filters are silent drops, and each one looks identical to "the event
                    // was never injected" from the client side. name which fired.
                    if sender == user {
                        crate::log(format_args!(
                            "sync {user}: skip seq={s} in {room_id} (we sent it)"
                        ));
                    } else if !member_of.contains(&room_id.as_str()) {
                        crate::log(format_args!(
                            "sync {user}: DROP seq={s} in {room_id} -- not a member (member of {member_of:?})"
                        ));
                    } else {
                        crate::log(format_args!("sync {user}: deliver seq={s} in {room_id}"));
                        by_room
                            .entry(room_id.clone())
                            .or_default()
                            .push(event.clone());
                    }
                }
                Delivery::ToDevice {
                    user: u,
                    device: d,
                    event,
                } => {
                    if u == user && d == device {
                        to_device.push(event.clone());
                    }
                }
            }
        }
        let changed: Vec<String> = log
            .key_changes
            .iter()
            .filter(|(s, u)| *s > cursor && u != user)
            .map(|(_, u)| u.clone())
            .collect();
        let seq = log.seq;
        drop(log);
        let mut joined = serde_json::Map::new();
        for (rid, evs) in by_room {
            joined.insert(
                rid,
                json!({"timeline": {"events": evs, "limited": false, "prev_batch": since}}),
            );
        }
        json!({
            "next_batch": format!("s{seq}"),
            "rooms": {"join": joined, "invite": {}, "leave": {}},
            "account_data": {"events": []},
            "device_lists": {"changed": dedup_sorted(changed), "left": []},
            "to_device": {"events": to_device},
        })
    }

    pub fn is_encrypted(&self, room_id: &str) -> bool {
        self.rooms.iter().any(|r| r.id == room_id && r.encrypted)
    }

    // m.direct account data FROM `user`'s perspective: each direct room they are in, filed under
    // its other member. lets the bot (and the helper) treat their 1:1 rooms as DMs -- the bridge
    // answers a DM unconditionally, so the encrypted DM does not need an explicit mention.
    fn direct_for(&self, user: &str) -> serde_json::Map<String, Value> {
        let mut m: HashMap<String, Vec<String>> = HashMap::new();
        for room in &self.rooms {
            if room.is_direct && room.members.iter().any(|x| x == user) {
                for other in room.members.iter().filter(|x| *x != user) {
                    m.entry(other.clone()).or_default().push(room.id.clone());
                }
            }
        }
        m.into_iter().map(|(k, v)| (k, json!(v))).collect()
    }

    // only the bot has a profile display name here; everyone else is nameless, which is itself a
    // real case (a user who never set one) and keeps the bot's name unambiguous in assertions.
    pub fn display_name_for(&self, user: &str) -> Option<&str> {
        (user == self.bot_user).then_some(self.bot_display_name.as_str())
    }

    pub fn members_for(&self, room_id: &str) -> Vec<Value> {
        self.rooms
            .iter()
            .find(|r| r.id == room_id)
            .map(|r| self.member_events(r))
            .unwrap_or_default()
    }

    pub fn rooms_snapshot(&self) -> Vec<Value> {
        self.rooms
            .iter()
            .map(|r| json!({"room_id": r.id, "members": r.members, "is_direct": r.is_direct, "encrypted": r.encrypted}))
            .collect()
    }

    fn member_events(&self, room: &Room) -> Vec<Value> {
        room.members
            .iter()
            .map(|u| {
                json!({
                    "type": "m.room.member", "sender": u, "state_key": u,
                    "content": {"membership": "join"},
                    "event_id": format!("$m_{}_{}", id_frag(&room.id), id_frag(u)),
                    "origin_server_ts": 0,
                })
            })
            .collect()
    }

    fn room_state(&self, room: &Room) -> Vec<Value> {
        let mut evs = vec![json!({
            "type": "m.room.create", "sender": room.creator, "state_key": "",
            "content": {"creator": room.creator},
            "event_id": format!("$create_{}", id_frag(&room.id)), "origin_server_ts": 0,
        })];
        evs.extend(self.member_events(room));
        if room.encrypted {
            evs.push(json!({
                "type": "m.room.encryption", "sender": room.creator, "state_key": "",
                "content": {"algorithm": "m.megolm.v1.aes-sha2"},
                "event_id": format!("$enc_{}", id_frag(&room.id)), "origin_server_ts": 0,
            }));
        }
        evs
    }
}

fn dedup_sorted(mut v: Vec<String>) -> Vec<String> {
    v.sort();
    v.dedup();
    v
}
