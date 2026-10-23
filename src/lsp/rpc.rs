//! json-rpc framing for lsp: `Content-Length: n\r\n\r\n` then n bytes of
//! json. what servers send is untrusted, so sizes are capped and bad frames
//! are errors, not panics.

use serde_json::{Value, json};

/// biggest message body taken from a server.
pub const MAX_BODY: usize = 64 * 1024 * 1024;
/// longest header block before the blank line.
const MAX_HEADER: usize = 4096;

fn frame(body: &Value) -> Vec<u8> {
    let body = body.to_string();
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body.as_bytes());
    out
}

pub fn request(id: i64, method: &str, params: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

pub fn notification(method: &str, params: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

/// answer to a request the server sent us.
pub fn response(id: &Value, result: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// cuts one whole message off the front of `buf`. none while it is still
/// arriving. an error means the stream is broken and should be dropped.
pub fn take(buf: &mut Vec<u8>) -> Result<Option<Value>, String> {
    let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else {
        if buf.len() > MAX_HEADER {
            return Err("header too long".to_owned());
        }
        return Ok(None);
    };
    if end > MAX_HEADER {
        return Err("header too long".to_owned());
    }
    let header = std::str::from_utf8(&buf[..end]).map_err(|_| "header is not text".to_owned())?;
    let mut length = None;
    for line in header.split("\r\n") {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse::<usize>().ok();
        }
    }
    let length = length.ok_or_else(|| "no Content-Length".to_owned())?;
    if length > MAX_BODY {
        return Err(format!("message of {length} bytes is too big"));
    }
    let start = end + 4;
    if buf.len() < start + length {
        return Ok(None);
    }
    let value = serde_json::from_slice(&buf[start..start + length]).map_err(|e| e.to_string());
    buf.drain(..start + length);
    value.map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_round_trip_in_pieces() {
        let mut stream = request(1, "initialize", json!({ "x": "가" }));
        stream.extend(notification("initialized", json!({})));
        let mut buf = Vec::new();
        let mut got = Vec::new();
        for byte in stream {
            buf.push(byte);
            while let Some(msg) = take(&mut buf).unwrap() {
                got.push(msg);
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0]["params"]["x"], "가");
        assert_eq!(got[1]["method"], "initialized");
        assert!(buf.is_empty());
    }

    #[test]
    fn bad_frames_are_errors() {
        let mut huge = format!("Content-Length: {}\r\n\r\n", MAX_BODY + 1).into_bytes();
        assert!(take(&mut huge).is_err());
        let mut none = b"X-Thing: 1\r\n\r\n{}".to_vec();
        assert!(take(&mut none).is_err());
        let mut endless = vec![b'a'; MAX_HEADER + 1];
        assert!(take(&mut endless).is_err());
        let mut junk = b"Content-Length: 3\r\n\r\n{{{".to_vec();
        assert!(take(&mut junk).is_err());
        assert!(junk.is_empty(), "the broken body is dropped");
    }
}
