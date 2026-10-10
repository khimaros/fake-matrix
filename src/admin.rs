// the /_fake control plane a test drives: inject an inbound message, read the bot's
// captured sends, list the seeded rooms, reset. mirrors fake-openai's /__admin.

use serde_json::{json, Value};

use crate::response::{hex_decode, HttpResponse, ParsedRequest};
use crate::state::{Media, Shared};

pub const PREFIX: &str = "/_fake";

// the file an inject carries, when every part of it is there and its bytes are hex.
fn media_of(v: &Value) -> Option<Media<'_>> {
    let field = |k: &str| v.get(k).and_then(|x| x.as_str());
    Some(Media {
        msgtype: field("msgtype")?,
        filename: field("filename")?,
        mimetype: field("mimetype")?,
        bytes: hex_decode(field("hex")?)?,
    })
}

pub fn handle(state: &Shared, req: &ParsedRequest, path: &str) -> HttpResponse {
    let sub = &path[PREFIX.len()..];
    match (req.method.as_str(), sub) {
        ("GET", "/health") => HttpResponse::json(
            200,
            &json!({"ok": true, "sent": state.sent_snapshot().len(), "rooms": state.rooms_snapshot().len()}),
        ),
        ("GET", "/sent") => HttpResponse::json(200, &json!({"sent": state.sent_snapshot()})),
        ("GET", "/receipts") => {
            HttpResponse::json(200, &json!({"receipts": state.receipts_snapshot()}))
        }
        ("GET", "/rooms") => HttpResponse::json(200, &json!({"rooms": state.rooms_snapshot()})),
        // syncs served per user. inject BEFORE a client's first sync and the event is lost for
        // good (initial sync = state, not backlog, and its token points past the injection), so a
        // test should wait on this before injecting rather than on the client's own "ready" log.
        ("GET", "/syncs") => HttpResponse::json(200, &json!({"syncs": state.sync_counts()})),
        ("POST", "/inject") => {
            let room_id = req.body.get("room_id").and_then(|v| v.as_str());
            let sender = req.body.get("sender").and_then(|v| v.as_str());
            let body = req.body.get("body").and_then(|v| v.as_str());
            // optional, and the only way to drive a real MENTION: the plain body renders a pill as
            // its text alone, so who was addressed lives in formatted_body + m.mentions.
            let formatted = req.body.get("formatted_body").and_then(|v| v.as_str());
            let mentions = req.body.get("mentions");
            // optional: the message is a file. {msgtype, filename, mimetype, hex}, and `body` is
            // then its caption (or the filename again, for none).
            let media = req.body.get("media").and_then(media_of);
            match (room_id, sender, body) {
                (Some(r), Some(s), Some(b)) => {
                    // the event id comes back so a test can assert what a REPLY points at.
                    let event_id = state.inject(r, s, b, formatted, mentions, media);
                    HttpResponse::json(200, &json!({"ok": true, "event_id": event_id}))
                }
                _ => HttpResponse::json(
                    400,
                    &json!({"error": "inject requires room_id, sender, body"}),
                ),
            }
        }
        ("POST", "/reset") => {
            state.reset();
            HttpResponse::json(200, &json!({"ok": true}))
        }
        _ => HttpResponse::json(
            404,
            &json!({"error": format!("unknown admin endpoint: {} {sub}", req.method)}),
        ),
    }
}
