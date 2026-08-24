//! Binary media envelope: first byte is the unit type.

pub const TYPE_INIT: u8 = 1;
pub const TYPE_FRAG: u8 = 2;
pub const TYPE_JPEG: u8 = 3;
/// Analysis snapshot for the Worker LLM. Viewers must ignore this type.
pub const TYPE_SNAP: u8 = 4;

pub fn pack_media(kind: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(kind);
    out.extend_from_slice(payload);
    out
}

pub fn unpack_media(data: &[u8]) -> Result<(u8, &[u8]), &'static str> {
    if data.is_empty() {
        return Err("empty media envelope");
    }
    Ok((data[0], &data[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_unpack_init_fragment_jpeg() {
        for (kind, blob) in [
            (TYPE_INIT, &b"ftyp....moov"[..]),
            (TYPE_FRAG, &b"moof....mdat"[..]),
            (TYPE_JPEG, &b"\xff\xd8\xff"[..]),
            (TYPE_SNAP, &b"\xff\xd8snap"[..]),
        ] {
            let packed = pack_media(kind, blob);
            assert_eq!(packed[0], kind);
            let (k, rest) = unpack_media(&packed).unwrap();
            assert_eq!(k, kind);
            assert_eq!(rest, blob);
        }
    }

    #[test]
    fn unpack_rejects_empty() {
        assert!(unpack_media(b"").is_err());
    }
}
