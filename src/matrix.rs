// the matrix client-server endpoints the fake serves. paths are matched by hand (split into
// segments) to avoid a regex dependency; the version segment (v3/r0) is wildcarded. beyond the
// basics a bot exercises, it relays the e2e key + to-device traffic two real clients produce so
// encrypted rooms work without the fake doing any crypto. any other path returns an empty 200.

use serde_json::json;

use crate::response::{percent_decode, query_param, HttpResponse, ParsedRequest};
use crate::state::{Shared, SUPPORTED_VERSIONS};

pub const PREFIX: &str = "/_matrix/";

pub fn handle(state: &Shared, req: &ParsedRequest, path: &str) -> HttpResponse {
    let decoded: Vec<String> = path
        .trim_matches('/')
        .split('/')
        .map(percent_decode)
        .collect();
    let segs: Vec<&str> = decoded.iter().map(String::as_str).collect();
    let body = &req.body;
    match (req.method.as_str(), segs.as_slice()) {
        ("GET", &["_matrix", "client", "versions"]) => HttpResponse::json(
            200,
            &json!({"versions": SUPPORTED_VERSIONS, "unstable_features": {}}),
        ),
        ("GET", &["_matrix", "client", _v, "login"]) => {
            HttpResponse::json(200, &json!({"flows": [{"type": "m.login.password"}]}))
        }
        ("POST", &["_matrix", "client", _v, "login"]) => {
            // a real login (used by the helper): mint a fresh device + token for the human.
            let user = body
                .get("identifier")
                .and_then(|i| i.get("user"))
                .or_else(|| body.get("user"))
                .and_then(|v| v.as_str())
                .unwrap_or("tester");
            let (user_id, device_id, token) = state.login(user);
            HttpResponse::json(
                200,
                &json!({
                    "user_id": user_id, "access_token": token, "device_id": device_id,
                    "home_server": "fake.local",
                }),
            )
        }
        ("GET", &["_matrix", "client", _v, "account", "whoami"]) => {
            let (user, device) = state.resolve(&req.token);
            HttpResponse::json(200, &json!({"user_id": user, "device_id": device}))
        }
        ("POST", &["_matrix", "client", _v, "keys", "upload"]) => {
            let device_keys = body.get("device_keys").cloned().unwrap_or(json!({}));
            let otks = body.get("one_time_keys").cloned().unwrap_or(json!({}));
            HttpResponse::json(200, &state.upload_keys(&req.token, &device_keys, &otks))
        }
        ("POST", &["_matrix", "client", _v, "keys", "query"]) => {
            let users: Vec<String> = body
                .get("device_keys")
                .and_then(|v| v.as_object())
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            HttpResponse::json(200, &state.query_keys(&users))
        }
        ("POST", &["_matrix", "client", _v, "keys", "claim"]) => {
            let reqs = body.get("one_time_keys").cloned().unwrap_or(json!({}));
            HttpResponse::json(200, &state.claim_keys(&reqs))
        }
        ("PUT", &["_matrix", "client", _v, "sendToDevice", etype, _txn]) => {
            let messages = body.get("messages").cloned().unwrap_or(json!({}));
            state.send_to_device(&req.token, etype, &messages);
            HttpResponse::json(200, &json!({}))
        }
        ("GET", &["_matrix", "client", _v, "sync"]) => {
            let since = query_param(&req.path, "since");
            HttpResponse::json(200, &state.sync(&req.token, since.as_deref()))
        }
        ("PUT", &["_matrix", "client", _v, "rooms", room, "send", etype, _txn]) => {
            let content = body.clone();
            let id = state.room_send(&req.token, room, etype, &content);
            HttpResponse::json(200, &json!({"event_id": id}))
        }
        ("PUT", &["_matrix", "client", _v, "rooms", _room, "typing", _user]) => {
            HttpResponse::json(200, &json!({}))
        }
        ("GET", &["_matrix", "client", _v, "rooms", room, "members"]) => {
            HttpResponse::json(200, &json!({"chunk": state.members_for(room)}))
        }
        // a read receipt. the response is an empty 200 either way; recording it is what lets a
        // test see that a bot acknowledged a message it chose not to reply to.
        ("POST", &["_matrix", "client", _v, "rooms", room, "receipt", rtype, event_id]) => {
            state.receipt(&req.token, room, rtype, event_id);
            HttpResponse::json(200, &json!({}))
        }
        // profile display name. a mention pill renders as this, so a bridge that strips mentions
        // reads it -- serving the bot's own tells a test the bot fetched it rather than guessed.
        ("GET", &["_matrix", "client", _v, "profile", user, "displayname"]) => HttpResponse::json(
            200,
            &json!({"displayname": state.display_name_for(user)}),
        ),
        // an encrypted room's m.room.encryption state, so a client that probes it (rather than
        // reading the event from sync) still sees the room as encrypted.
        ("GET", &["_matrix", "client", _, "rooms", room, "state", "m.room.encryption"])
            if state.is_encrypted(room) =>
        {
            HttpResponse::json(200, &json!({"algorithm": "m.megolm.v1.aes-sha2"}))
        }
        // any other state-event lookup is for an event the fake never stores: a matrix M_NOT_FOUND,
        // so the sdk resolves the room as UNENCRYPTED (an empty 200 would read as a redacted
        // m.room.encryption event, and send() would then fail with EncryptionNotEnabled).
        ("GET", &["_matrix", "client", _, "rooms", _, "state", _])
        | ("GET", &["_matrix", "client", _, "rooms", _, "state", _, _]) => HttpResponse::json(
            404,
            &json!({"errcode": "M_NOT_FOUND", "error": "event not found"}),
        ),
        _ => HttpResponse::json(200, &json!({})),
    }
}
