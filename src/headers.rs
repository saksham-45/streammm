//! HTTP streaming headers: chunked, unbuffered, no transform.

use std::collections::BTreeMap;

/// Headers applied to `/stream.mp4` and `/stream.mjpeg`.
pub fn stream_header_map() -> BTreeMap<&'static str, &'static str> {
    let mut m = BTreeMap::new();
    m.insert(
        "Cache-Control",
        "no-store, no-cache, no-transform, must-revalidate",
    );
    m.insert("CDN-Cache-Control", "no-store");
    m.insert("X-Accel-Buffering", "no");
    m.insert("Transfer-Encoding", "chunked");
    m
}

/// RFC 9112 chunk. Empty payload is the terminator `0\r\n\r\n`.
pub fn encode_chunk(data: &[u8]) -> Vec<u8> {
    if data.is_empty() {
        return b"0\r\n\r\n".to_vec();
    }
    let mut out = format!("{:X}\r\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\r\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_store_no_transform_chunked() {
        let h = stream_header_map();
        let cc = h.get("Cache-Control").unwrap().to_lowercase();
        assert!(cc.contains("no-store"), "{cc}");
        assert!(cc.contains("no-transform"), "{cc}");
        assert_eq!(
            h.get("CDN-Cache-Control").unwrap().to_lowercase(),
            "no-store"
        );
        assert_eq!(h.get("X-Accel-Buffering").unwrap().to_lowercase(), "no");
        assert_eq!(
            h.get("Transfer-Encoding").unwrap().to_lowercase(),
            "chunked"
        );
    }

    #[test]
    fn chunk_framing() {
        assert_eq!(encode_chunk(b"hello"), b"5\r\nhello\r\n");
        assert_eq!(encode_chunk(b""), b"0\r\n\r\n");
        let payload = vec![b'a'; 16];
        let wire = encode_chunk(&payload);
        assert!(wire.starts_with(b"10\r\n"));
        assert!(wire.ends_with(b"\r\n"));
        assert_eq!(&wire[4..wire.len() - 2], payload.as_slice());
    }
}
