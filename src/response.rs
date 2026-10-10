// http message types: the parsed inbound request and the outbound response.
// every response is a buffered json body (no streaming needed for the matrix
// client-server api this fake serves).

use serde_json::{json, Value};
use tiny_http::{Header, Request, Response};

pub const CONTENT_TYPE_JSON: &str = "application/json";

// an inbound request after parsing. path retains any query string (the /sync
// `since` cursor rides there); body parsed as json (or Null when absent/invalid).
// `token` is the bearer access token (empty if none), used to resolve which device
// is calling -- the e2e relay keys per-device state (to-device queues, uploaded keys) on it.
pub struct ParsedRequest {
    pub method: String,
    pub path: String,
    pub body: Value,
    pub token: String,
}

pub fn parse_request(req: &mut Request) -> ParsedRequest {
    let method = req.method().as_str().to_string();
    let path = req.url().to_string();
    let token = req
        .headers()
        .iter()
        .find(|h| h.field.equiv("Authorization"))
        .map(|h| h.value.as_str().trim_start_matches("Bearer ").to_string())
        .unwrap_or_default();
    let mut buf = Vec::new();
    let _ = req.as_reader().read_to_end(&mut buf);
    let body = serde_json::from_slice(&buf).unwrap_or(Value::Null);
    ParsedRequest {
        method,
        path,
        body,
        token,
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: String,
}

impl HttpResponse {
    pub fn json(status: u16, value: &Value) -> HttpResponse {
        HttpResponse {
            status,
            body: value.to_string().into_bytes(),
            content_type: CONTENT_TYPE_JSON.to_string(),
        }
    }

    // stored media, served as the bytes and the type it was injected with.
    pub fn bytes(content_type: &str, body: Vec<u8>) -> HttpResponse {
        HttpResponse {
            status: 200,
            body,
            content_type: content_type.to_string(),
        }
    }

    pub fn not_found(method: &str, path: &str) -> HttpResponse {
        HttpResponse::json(
            404,
            &json!({"error": format!("not found: {method} {path}")}),
        )
    }
}

pub fn write_response(req: Request, resp: HttpResponse) {
    let content_type = resp.content_type;
    let mut out = Response::from_data(resp.body).with_status_code(resp.status);
    out.add_header(
        Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
            .expect("valid header"),
    );
    // a client that hangs up mid-write is normal in tests; ignore the error.
    let _ = req.respond(out);
}

// percent-decode a single path segment. matrix clients encode room ids in paths
// (!dm:fake.local -> %21dm%3Afake.local), so the room segment must be decoded before
// it is matched against seeded room ids. segments never contain an encoded '/', so
// decoding after the split is safe.
pub fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex_val(b[i + 1]), hex_val(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// decode a hex string into bytes; none when it is not hex. how a test hands media to the admin
// plane without this crate taking a base64 dependency.
pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    let b = s.as_bytes();
    if b.len() % 2 != 0 {
        return None;
    }
    b.chunks(2)
        .map(|p| Some(hex_val(p[0])? * 16 + hex_val(p[1])?))
        .collect()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

// pull a query param off a full path (e.g. the /sync `since` cursor).
pub fn query_param(path: &str, key: &str) -> Option<String> {
    let (_, q) = path.split_once('?')?;
    for pair in q.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v.to_string());
            }
        }
    }
    None
}
