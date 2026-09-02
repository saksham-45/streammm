//! Watcher microphone → host speakers. Tests use FakeVoice; macOS uses afplay.

use parking_lot::Mutex;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;

pub const VOICE_MAX: usize = 48 * 1024;
pub const VOICE_RATE: u32 = 16_000;

pub fn clamp_rate(rate: u32) -> u32 {
    match rate {
        8_000 | 16_000 | 24_000 => rate,
        _ => VOICE_RATE,
    }
}

pub fn accept_pcm(pcm: Vec<u8>) -> Option<Vec<u8>> {
    if pcm.len() < 2 || pcm.len() > VOICE_MAX || pcm.len() % 2 != 0 {
        None
    } else {
        Some(pcm)
    }
}

pub fn wav_from_pcm_s16le(pcm: &[u8], rate: u32) -> Vec<u8> {
    let rate = clamp_rate(rate);
    let data_len = pcm.len() as u32;
    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * 2).to_le_bytes());
    out.extend_from_slice(&2u16.to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

pub trait VoiceSink: Send + Sync {
    fn play_pcm(&self, pcm: &[u8], rate: u32);
}

#[derive(Clone, Default)]
pub struct FakeVoice {
    pub takes: Arc<Mutex<Vec<(u32, Vec<u8>)>>>,
}

impl FakeVoice {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn recorded(&self) -> Vec<(u32, Vec<u8>)> {
        self.takes.lock().clone()
    }
}

impl VoiceSink for FakeVoice {
    fn play_pcm(&self, pcm: &[u8], rate: u32) {
        self.takes.lock().push((clamp_rate(rate), pcm.to_vec()));
    }
}

pub struct NullVoice;

impl VoiceSink for NullVoice {
    fn play_pcm(&self, _pcm: &[u8], _rate: u32) {}
}

pub struct AfplayVoice {
    dir: PathBuf,
    seq: AtomicU64,
    inflight: Arc<AtomicU32>,
}

impl AfplayVoice {
    pub fn new() -> Self {
        let dir = std::env::temp_dir().join("streamaid-voice");
        let _ = fs::create_dir_all(&dir);
        Self {
            dir,
            seq: AtomicU64::new(1),
            inflight: Arc::new(AtomicU32::new(0)),
        }
    }
}

impl VoiceSink for AfplayVoice {
    fn play_pcm(&self, pcm: &[u8], rate: u32) {
        if self.inflight.load(Ordering::Relaxed) >= 3 {
            return;
        }
        let wav = wav_from_pcm_s16le(pcm, rate);
        let n = self.seq.fetch_add(1, Ordering::Relaxed);
        let path = self.dir.join(format!("v{n}.wav"));
        if fs::write(&path, &wav).is_err() {
            return;
        }
        self.inflight.fetch_add(1, Ordering::Relaxed);
        let inflight = self.inflight.clone();
        thread::spawn(move || {
            let _ = Command::new("afplay").arg(&path).arg("-q").arg("1").status();
            let _ = fs::remove_file(&path);
            inflight.fetch_sub(1, Ordering::Relaxed);
        });
    }
}

pub fn production_voice() -> Arc<dyn VoiceSink> {
    if cfg!(target_os = "macos") {
        Arc::new(AfplayVoice::new())
    } else {
        Arc::new(NullVoice)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_44_bytes_and_pcm_follows() {
        let pcm = vec![0u8, 1, 2, 3];
        let wav = wav_from_pcm_s16le(&pcm, 16_000);
        assert_eq!(&wav[..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(&wav[44..], pcm);
        assert_eq!(u32::from_le_bytes(wav[24..28].try_into().unwrap()), 16_000);
    }

    #[test]
    fn accept_pcm_rejects_empty_odd_and_huge() {
        assert!(accept_pcm(vec![]).is_none());
        assert!(accept_pcm(vec![1]).is_none());
        assert!(accept_pcm(vec![1, 2]).is_some());
        assert!(accept_pcm(vec![0; VOICE_MAX + 2]).is_none());
        assert_eq!(clamp_rate(48_000), VOICE_RATE);
        assert_eq!(clamp_rate(8_000), 8_000);
    }

    #[test]
    fn fake_voice_records_play_calls() {
        let v = FakeVoice::new();
        v.play_pcm(&[9, 8], 16_000);
        let rec = v.recorded();
        assert_eq!(rec.len(), 1);
        assert_eq!(rec[0].0, 16_000);
        assert_eq!(rec[0].1, vec![9, 8]);
    }
}
