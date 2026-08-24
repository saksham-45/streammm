//! Split ffmpeg stdout into complete JPEG frames or fMP4 init/fragments.

const MAX_BUF: usize = 20 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unit {
    Jpeg(Vec<u8>),
    Init(Vec<u8>),
    Fragment(Vec<u8>),
}

pub fn jpeg_size(data: &[u8]) -> (u32, u32) {
    let n = data.len();
    if n < 4 || data[0] != 0xff || data[1] != 0xd8 {
        return (0, 0);
    }
    let mut i = 2usize;
    while i + 9 <= n {
        if data[i] != 0xff {
            i += 1;
            continue;
        }
        let m = data[i + 1];
        if m == 0xc0 || m == 0xc2 {
            let h = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let w = u16::from_be_bytes([data[i + 7], data[i + 8]]) as u32;
            return (w, h);
        }
        if m == 0xd8 || m == 0x01 || (0xd0..=0xd7).contains(&m) {
            i += 2;
            continue;
        }
        if i + 4 > n {
            return (0, 0);
        }
        let seglen = u16::from_be_bytes([data[i + 2], data[i + 3]]) as usize;
        if seglen < 2 {
            return (0, 0);
        }
        i += 2 + seglen;
    }
    (0, 0)
}

fn scan_eoi(buf: &[u8], start: usize, mut in_scan: bool) -> (Option<usize>, usize, bool) {
    let n = buf.len();
    let mut i = start;
    while i + 1 < n {
        if buf[i] != 0xff {
            i += 1;
            continue;
        }
        let m = buf[i + 1];
        if m == 0xd9 {
            return (Some(i), i + 2, false);
        }
        if in_scan {
            if m == 0x00 || (0xd0..=0xd7).contains(&m) {
                i += 2;
                continue;
            }
            return (None, (i + 2).min(n.saturating_sub(1)), true);
        }
        if m == 0xd8 || m == 0x01 || (0xd0..=0xd7).contains(&m) {
            i += 2;
            continue;
        }
        if m == 0xda {
            in_scan = true;
            i += 2;
            continue;
        }
        if i + 4 > n {
            return (None, i, false);
        }
        let seglen = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if seglen < 2 {
            return (None, i, false);
        }
        i += 2 + seglen;
    }
    (None, i, in_scan)
}

pub struct MjpegFramer {
    buf: Vec<u8>,
    scanning: bool,
    scan_from: usize,
    in_scan: bool,
}

impl Default for MjpegFramer {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            scanning: true,
            scan_from: 0,
            in_scan: false,
        }
    }
}

impl MjpegFramer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Vec<u8>> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            if self.scanning {
                let idx = find_soi(&self.buf, self.scan_from);
                if idx.is_none() {
                    self.scan_from = self.buf.len().saturating_sub(1);
                    break;
                }
                let idx = idx.unwrap();
                self.buf.drain(..idx);
                self.scanning = false;
                self.scan_from = 0;
                self.in_scan = false;
                continue;
            }
            let (eoi, resume, in_scan) = scan_eoi(&self.buf, self.scan_from, self.in_scan);
            self.scan_from = resume;
            self.in_scan = in_scan;
            match eoi {
                None => {
                    if self.buf.len() > MAX_BUF {
                        self.buf.clear();
                        self.scanning = true;
                        self.scan_from = 0;
                        self.in_scan = false;
                    }
                    break;
                }
                Some(idx) => {
                    let frame = self.buf[..idx + 2].to_vec();
                    self.buf.drain(..idx + 2);
                    self.scanning = true;
                    self.scan_from = 0;
                    self.in_scan = false;
                    out.push(frame);
                }
            }
        }
        out
    }
}

fn find_soi(buf: &[u8], start: usize) -> Option<usize> {
    if buf.len() < 2 {
        return None;
    }
    let from = start.min(buf.len());
    buf[from..]
        .windows(2)
        .position(|w| w == [0xff, 0xd8])
        .map(|p| from + p)
}

fn box_size_type(buf: &[u8]) -> Option<(usize, [u8; 4])> {
    if buf.len() < 8 {
        return None;
    }
    let size = u32::from_be_bytes(buf[0..4].try_into().ok()?) as usize;
    if size < 8 {
        return None;
    }
    let mut typ = [0u8; 4];
    typ.copy_from_slice(&buf[4..8]);
    Some((size, typ))
}

fn mp4_init_end(buf: &[u8]) -> Result<usize, i32> {
    if buf.len() < 8 || &buf[4..8] != b"ftyp" {
        return Err(-1);
    }
    let mut pos = u32::from_be_bytes(buf[0..4].try_into().unwrap()) as usize;
    if pos < 8 {
        return Err(-1);
    }
    if pos > buf.len() {
        return Err(0); // need more
    }
    loop {
        if pos + 8 > buf.len() {
            return Err(0);
        }
        let size = u32::from_be_bytes(buf[pos..pos + 4].try_into().unwrap()) as usize;
        let typ = &buf[pos + 4..pos + 8];
        if size < 8 {
            return Err(-1);
        }
        if typ == b"moov" {
            if buf.len() >= pos + size {
                return Ok(pos + size);
            }
            return Err(0);
        }
        if typ == b"free" || typ == b"skip" || typ == b"wide" {
            pos += size;
            continue;
        }
        return Err(-1);
    }
}

fn next_fragment(buf: &[u8]) -> Option<usize> {
    let (size, typ) = box_size_type(buf)?;
    if typ != *b"moof" || buf.len() < size {
        return None;
    }
    let after = size;
    if after + 8 > buf.len() {
        return None;
    }
    let msize = u32::from_be_bytes(buf[after..after + 4].try_into().ok()?) as usize;
    if msize < 8 {
        return None;
    }
    let mtyp = &buf[after + 4..after + 8];
    if mtyp == b"mdat" {
        if buf.len() < size + msize {
            return None;
        }
        return Some(size + msize);
    }
    Some(size)
}

#[derive(Default)]
pub struct Mp4Framer {
    buf: Vec<u8>,
    seen_init: bool,
}

impl Mp4Framer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, chunk: &[u8]) -> Vec<Unit> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        if !self.seen_init {
            match mp4_init_end(&self.buf) {
                Err(-1) | Err(0) if self.buf.len() > MAX_BUF => {
                    out.push(Unit::Fragment(std::mem::take(&mut self.buf)));
                    self.seen_init = true;
                }
                Err(_) => return out,
                Ok(end) => {
                    let init = self.buf[..end].to_vec();
                    self.buf.drain(..end);
                    self.seen_init = true;
                    out.push(Unit::Init(init));
                }
            }
        }
        while let Some(frag) = next_fragment(&self.buf) {
            let f = self.buf[..frag].to_vec();
            self.buf.drain(..frag);
            out.push(Unit::Fragment(f));
        }
        if self.buf.len() > MAX_BUF {
            self.buf.clear();
        }
        out
    }
}

/// Build a complete ISO-BMFF box (test helper used by tests and optional tools).
pub fn mp4_box(typ: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let size = 8 + payload.len() as u32;
    let mut v = Vec::with_capacity(size as usize);
    v.extend_from_slice(&size.to_be_bytes());
    v.extend_from_slice(typ);
    v.extend_from_slice(payload);
    v
}

pub fn make_jpeg(w: u16, h: u16, entropy: &[u8]) -> Vec<u8> {
    let mut v = vec![0xff, 0xd8];
    // SOF0
    let mut sof = vec![0x08];
    sof.extend_from_slice(&h.to_be_bytes());
    sof.extend_from_slice(&w.to_be_bytes());
    sof.extend_from_slice(&[1, 1, 0x11, 0]); // 1 component
    let sof_len = (sof.len() + 2) as u16;
    v.extend_from_slice(&[0xff, 0xc0]);
    v.extend_from_slice(&sof_len.to_be_bytes());
    v.extend_from_slice(&sof);
    let sos = [1u8, 1, 0x00, 0, 0x3f, 0];
    let sos_len = (sos.len() + 2) as u16;
    v.extend_from_slice(&[0xff, 0xda]);
    v.extend_from_slice(&sos_len.to_be_bytes());
    v.extend_from_slice(&sos);
    v.extend_from_slice(entropy);
    v.extend_from_slice(&[0xff, 0xd9]);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_concatenated_jpegs_at_eoi() {
        let a = make_jpeg(16, 10, b"AAA");
        let b = make_jpeg(32, 20, b"BBBB");
        assert_eq!(jpeg_size(&a), (16, 10));
        assert_eq!(jpeg_size(&b), (32, 20));
        let mut f = MjpegFramer::new();
        let mut concat = a.clone();
        concat.extend_from_slice(&b);
        // split across a push boundary inside the second JPEG
        let mid = a.len() + 4;
        let mut frames = f.push(&concat[..mid]);
        frames.extend(f.push(&concat[mid..]));
        assert_eq!(frames.len(), 2, "expected 2 JPEGs, got {}", frames.len());
        assert_eq!(frames[0], a);
        assert_eq!(frames[1], b);
    }

    #[test]
    fn splits_init_then_fragments_from_concatenated_boxes() {
        let ftyp = mp4_box(b"ftyp", b"isom");
        let moov = mp4_box(b"moov", b"trak-payload");
        let mut init = ftyp;
        init.extend_from_slice(&moov);
        let frag1 = {
            let mut v = mp4_box(b"moof", b"mfhd");
            v.extend_from_slice(&mp4_box(b"mdat", b"frame-one"));
            v
        };
        let frag2 = {
            let mut v = mp4_box(b"moof", b"mfhd2");
            v.extend_from_slice(&mp4_box(b"mdat", b"frame-two-xx"));
            v
        };
        let mut all = init.clone();
        all.extend_from_slice(&frag1);
        all.extend_from_slice(&frag2);
        let mut p = Mp4Framer::new();
        let cut = init.len() + 10;
        let mut units = p.push(&all[..cut]);
        units.extend(p.push(&all[cut..]));
        assert_eq!(units.len(), 3);
        match &units[0] {
            Unit::Init(b) => assert_eq!(b, &init),
            other => panic!("expected init, got {other:?}"),
        }
        match &units[1] {
            Unit::Fragment(b) => assert_eq!(b, &frag1),
            other => panic!("expected frag1, got {other:?}"),
        }
        match &units[2] {
            Unit::Fragment(b) => assert_eq!(b, &frag2),
            other => panic!("expected frag2, got {other:?}"),
        }
    }
}
