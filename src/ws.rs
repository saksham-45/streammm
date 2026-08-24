//! RFC 6455 accept-key and binary frame codec (used by `/stream.ws`).

use sha1::{Digest, Sha1};

pub const WS_MAGIC: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
pub const OP_CONT: u8 = 0;
pub const OP_TEXT: u8 = 1;
pub const OP_BIN: u8 = 2;
pub const OP_CLOSE: u8 = 8;
pub const OP_PING: u8 = 9;
pub const OP_PONG: u8 = 10;

pub fn accept_key(key: &str) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    let mut h = Sha1::new();
    h.update(key.as_bytes());
    h.update(WS_MAGIC.as_bytes());
    STANDARD.encode(h.finalize())
}

pub fn encode_frame(payload: &[u8], opcode: u8, masked: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(14 + payload.len());
    out.push(0x80 | (opcode & 0x0f));
    let n = payload.len();
    let mut b1 = if masked { 0x80 } else { 0 };
    if n < 126 {
        b1 |= n as u8;
        out.push(b1);
    } else if n < 65536 {
        b1 |= 126;
        out.push(b1);
        out.extend_from_slice(&(n as u16).to_be_bytes());
    } else {
        b1 |= 127;
        out.push(b1);
        out.extend_from_slice(&(n as u64).to_be_bytes());
    }
    let mut data = payload.to_vec();
    if masked {
        let mut key = [0u8; 4];
        let _ = getrandom::getrandom(&mut key);
        out.extend_from_slice(&key);
        for (i, b) in data.iter_mut().enumerate() {
            *b ^= key[i % 4];
        }
    }
    out.extend_from_slice(&data);
    out
}

/// Returns `(opcode, payload, consumed)`. `opcode` is `None` if incomplete.
pub fn decode_frame(buf: &[u8]) -> (Option<u8>, Vec<u8>, usize) {
    if buf.len() < 2 {
        return (None, Vec::new(), 0);
    }
    let opcode = buf[0] & 0x0f;
    let masked = buf[1] & 0x80 != 0;
    let mut len = (buf[1] & 0x7f) as usize;
    let mut i = 2usize;
    if len == 126 {
        if buf.len() < 4 {
            return (None, Vec::new(), 0);
        }
        len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        i = 4;
    } else if len == 127 {
        if buf.len() < 10 {
            return (None, Vec::new(), 0);
        }
        let n = u64::from_be_bytes(buf[2..10].try_into().unwrap());
        len = n as usize;
        i = 10;
    }
    let mut mask = [0u8; 4];
    if masked {
        if buf.len() < i + 4 {
            return (None, Vec::new(), 0);
        }
        mask.copy_from_slice(&buf[i..i + 4]);
        i += 4;
    }
    if buf.len() < i + len {
        return (None, Vec::new(), 0);
    }
    let mut payload = buf[i..i + len].to_vec();
    if masked {
        for (k, b) in payload.iter_mut().enumerate() {
            *b ^= mask[k % 4];
        }
    }
    (Some(opcode), payload, i + len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc6455_accept_key_example() {
        let key = "dGhlIHNhbXBsZSBub25jZQ==";
        assert_eq!(accept_key(key), "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
    }

    #[test]
    fn unmasked_binary_roundtrip() {
        let payload = [1u8].into_iter().chain(b"moofxxxx".iter().copied()).collect::<Vec<_>>();
        let wire = encode_frame(&payload, OP_BIN, false);
        let (op, data, n) = decode_frame(&wire);
        assert_eq!(op, Some(OP_BIN));
        assert_eq!(data, payload);
        assert_eq!(n, wire.len());
    }

    #[test]
    fn masked_client_frame_unmasks() {
        let payload = b"hello-ws";
        let wire = encode_frame(payload, OP_BIN, true);
        let (op, data, n) = decode_frame(&wire);
        assert_eq!(op, Some(OP_BIN));
        assert_eq!(data, payload);
        assert_eq!(n, wire.len());
    }

    #[test]
    fn ping_pong_opcodes() {
        let ping = encode_frame(b"hi", OP_PING, false);
        let (op, data, _) = decode_frame(&ping);
        assert_eq!(op, Some(OP_PING));
        assert_eq!(data, b"hi");
        let pong = encode_frame(&data, OP_PONG, false);
        assert_eq!(decode_frame(&pong).0, Some(OP_PONG));
    }

    #[test]
    fn incomplete_frame_returns_none() {
        let wire = encode_frame(b"abcdef", OP_BIN, false);
        let (op, _, n) = decode_frame(&wire[..2.min(wire.len())]);
        if wire.len() > 2 {
            assert_eq!(op, None);
            assert_eq!(n, 0);
        }
    }

    #[test]
    fn extended_payload_length_16bit() {
        let payload = vec![b'x'; 200];
        let wire = encode_frame(&payload, OP_BIN, false);
        let (op, data, n) = decode_frame(&wire);
        assert_eq!(op, Some(OP_BIN));
        assert_eq!(data, payload);
        assert_eq!(n, wire.len());
    }
}
