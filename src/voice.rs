//! Watcher microphone → host speakers. Tests use FakeVoice; live uses a persistent ffmpeg/ffplay pipe.

use parking_lot::Mutex;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

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

/// Mute the default input while Talk plays so Capture audio does not loop the speakers.
pub fn should_duck_mic(capture_audio: bool) -> bool {
    capture_audio
}

pub const MIC_DUCK_HOLD: Duration = Duration::from_millis(750);

pub trait MicDuck: Send + Sync {
    fn set_ducked(&self, _on: bool) {}
    fn is_ducked(&self) -> bool {
        false
    }
}

#[derive(Clone, Default)]
pub struct FakeDuck {
    pub ducked: Arc<AtomicBool>,
}

impl FakeDuck {
    pub fn new() -> Self {
        Self::default()
    }
}

impl MicDuck for FakeDuck {
    fn set_ducked(&self, on: bool) {
        self.ducked.store(on, Ordering::SeqCst);
    }
    fn is_ducked(&self) -> bool {
        self.ducked.load(Ordering::SeqCst)
    }
}

pub struct NullDuck;

impl MicDuck for NullDuck {}

pub fn production_duck() -> Arc<dyn MicDuck> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(macos_duck::MacDuck::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(NullDuck)
    }
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

pub fn ffmpeg_audiotoolbox_argv(rate: u32) -> Vec<String> {
    let rate = clamp_rate(rate).to_string();
    vec![
        "-nostdin".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "s16le".into(),
        "-ar".into(),
        rate,
        "-ac".into(),
        "1".into(),
        "-i".into(),
        "pipe:0".into(),
        "-f".into(),
        "audiotoolbox".into(),
        "default".into(),
    ]
}

pub fn ffplay_voice_argv(rate: u32) -> Vec<String> {
    let rate = clamp_rate(rate).to_string();
    vec![
        "-nodisp".into(),
        "-loglevel".into(),
        "error".into(),
        "-fflags".into(),
        "nobuffer".into(),
        "-flags".into(),
        "low_delay".into(),
        "-probesize".into(),
        "32".into(),
        "-analyzeduration".into(),
        "0".into(),
        "-f".into(),
        "s16le".into(),
        "-ar".into(),
        rate,
        "-ac".into(),
        "1".into(),
        "-i".into(),
        "pipe:0".into(),
    ]
}

pub fn voice_player_candidates(rate: u32) -> Vec<(&'static str, Vec<String>)> {
    let mut out = Vec::new();
    if cfg!(target_os = "macos") {
        out.push(("ffmpeg", ffmpeg_audiotoolbox_argv(rate)));
    }
    out.push(("ffplay", ffplay_voice_argv(rate)));
    out
}

struct PipeInner {
    child: Option<Child>,
    stdin: Option<ChildStdin>,
    rate: u32,
}

pub struct PipeVoice {
    inner: Mutex<PipeInner>,
    fallback: AfplayVoice,
}

impl PipeVoice {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PipeInner {
                child: None,
                stdin: None,
                rate: 0,
            }),
            fallback: AfplayVoice::new(),
        }
    }

    #[cfg(test)]
    fn spawned(program: &str, args: &[String], stdout: Stdio) -> Option<Self> {
        let mut cmd = Command::new(program);
        cmd.args(args)
            .stdin(Stdio::piped())
            .stdout(stdout)
            .stderr(Stdio::null());
        let mut child = cmd.spawn().ok()?;
        let stdin = child.stdin.take()?;
        Some(Self {
            inner: Mutex::new(PipeInner {
                child: Some(child),
                stdin: Some(stdin),
                rate: VOICE_RATE,
            }),
            fallback: AfplayVoice::new(),
        })
    }

    fn ensure_pipe(inner: &mut PipeInner, rate: u32) -> bool {
        if inner.stdin.is_some() && inner.rate == rate {
            if let Some(child) = inner.child.as_mut() {
                if child.try_wait().ok().flatten().is_none() {
                    return true;
                }
            }
        }
        inner.stdin.take();
        if let Some(mut child) = inner.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        for (bin, args) in voice_player_candidates(rate) {
            let mut cmd = Command::new(bin);
            cmd.args(&args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            let Ok(mut child) = cmd.spawn() else {
                continue;
            };
            thread::sleep(Duration::from_millis(30));
            if child.try_wait().ok().flatten().is_some() {
                continue;
            }
            let Some(stdin) = child.stdin.take() else {
                let _ = child.kill();
                continue;
            };
            inner.child = Some(child);
            inner.stdin = Some(stdin);
            inner.rate = rate;
            return true;
        }
        false
    }
}

impl PipeVoice {
    fn write_pcm(inner: &mut PipeInner, pcm: &[u8]) -> bool {
        inner
            .stdin
            .as_mut()
            .map(|s| s.write_all(pcm).is_ok() && s.flush().is_ok())
            .unwrap_or(false)
    }

    fn pipe_alive(inner: &mut PipeInner, rate: u32) -> bool {
        inner.stdin.is_some()
            && inner.rate == rate
            && inner
                .child
                .as_mut()
                .map(|c| c.try_wait().ok().flatten().is_none())
                .unwrap_or(false)
    }
}

impl VoiceSink for PipeVoice {
    fn play_pcm(&self, pcm: &[u8], rate: u32) {
        let rate = clamp_rate(rate);
        let mut g = self.inner.lock();
        if !Self::pipe_alive(&mut g, rate) && !Self::ensure_pipe(&mut g, rate) {
            drop(g);
            self.fallback.play_pcm(pcm, rate);
            return;
        }
        if !Self::write_pcm(&mut g, pcm) {
            g.stdin.take();
            if let Some(mut child) = g.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
            drop(g);
            self.fallback.play_pcm(pcm, rate);
        }
    }
}

impl Drop for PipeVoice {
    fn drop(&mut self) {
        let mut g = self.inner.lock();
        g.stdin.take();
        if let Some(mut child) = g.child.take() {
            for _ in 0..25 {
                if child.try_wait().ok().flatten().is_some() {
                    return;
                }
                thread::sleep(Duration::from_millis(10));
            }
            let _ = child.kill();
            let _ = child.wait();
        }
    }
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
    Arc::new(PipeVoice::new())
}

#[cfg(target_os = "macos")]
mod macos_duck {
    use super::MicDuck;
    use parking_lot::Mutex;
    use std::os::raw::c_void;
    use std::sync::atomic::{AtomicBool, Ordering};

    const SYSTEM: u32 = 1;
    const DEFAULT_INPUT: u32 = u32::from_be_bytes(*b"dIn ");
    const SCOPE_GLOBAL: u32 = u32::from_be_bytes(*b"glob");
    const SCOPE_INPUT: u32 = u32::from_be_bytes(*b"inpt");
    const VIRTUAL_VOL: u32 = u32::from_be_bytes(*b"vmvc");
    const VOLUME_SCALAR: u32 = u32::from_be_bytes(*b"volm");

    #[repr(C)]
    struct Addr {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    extern "C" {
        fn AudioObjectGetPropertyData(
            object: u32,
            address: *const Addr,
            qualifier_size: u32,
            qualifier: *const c_void,
            io_size: *mut u32,
            out: *mut c_void,
        ) -> i32;
        fn AudioObjectSetPropertyData(
            object: u32,
            address: *const Addr,
            qualifier_size: u32,
            qualifier: *const c_void,
            data_size: u32,
            data: *const c_void,
        ) -> i32;
        fn AudioObjectHasProperty(object: u32, address: *const Addr) -> bool;
    }

    fn default_input() -> Option<u32> {
        let addr = Addr {
            selector: DEFAULT_INPUT,
            scope: SCOPE_GLOBAL,
            element: 0,
        };
        let mut id = 0u32;
        let mut size = 4u32;
        let err = unsafe {
            AudioObjectGetPropertyData(
                SYSTEM,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut id as *mut u32 as *mut c_void,
            )
        };
        if err == 0 && id != 0 {
            Some(id)
        } else {
            None
        }
    }

    fn try_get(id: u32, selector: u32, element: u32) -> Option<f32> {
        let addr = Addr {
            selector,
            scope: SCOPE_INPUT,
            element,
        };
        if !unsafe { AudioObjectHasProperty(id, &addr) } {
            return None;
        }
        let mut v = 0f32;
        let mut size = 4u32;
        let err = unsafe {
            AudioObjectGetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                &mut size,
                &mut v as *mut f32 as *mut c_void,
            )
        };
        if err == 0 {
            Some(v.clamp(0.0, 1.0))
        } else {
            None
        }
    }

    fn try_set(id: u32, selector: u32, element: u32, v: f32) -> bool {
        let addr = Addr {
            selector,
            scope: SCOPE_INPUT,
            element,
        };
        if !unsafe { AudioObjectHasProperty(id, &addr) } {
            return false;
        }
        let v = v.clamp(0.0, 1.0);
        unsafe {
            AudioObjectSetPropertyData(
                id,
                &addr,
                0,
                std::ptr::null(),
                4,
                &v as *const f32 as *const c_void,
            ) == 0
        }
    }

    fn get_volume() -> Option<f32> {
        let id = default_input()?;
        try_get(id, VIRTUAL_VOL, 0)
            .or_else(|| try_get(id, VOLUME_SCALAR, 0))
            .or_else(|| try_get(id, VOLUME_SCALAR, 1))
    }

    fn set_volume(v: f32) -> bool {
        let Some(id) = default_input() else {
            return false;
        };
        try_set(id, VIRTUAL_VOL, 0, v)
            || try_set(id, VOLUME_SCALAR, 0, v)
            || try_set(id, VOLUME_SCALAR, 1, v)
    }

    pub struct MacDuck {
        saved: Mutex<Option<f32>>,
        on: AtomicBool,
    }

    impl MacDuck {
        pub fn new() -> Self {
            Self {
                saved: Mutex::new(None),
                on: AtomicBool::new(false),
            }
        }
    }

    impl MicDuck for MacDuck {
        fn set_ducked(&self, on: bool) {
            let mut saved = self.saved.lock();
            if on {
                if saved.is_some() {
                    return;
                }
                let Some(v) = get_volume() else {
                    return;
                };
                if !set_volume(0.0) {
                    return;
                }
                *saved = Some(v);
                self.on.store(true, Ordering::SeqCst);
            } else if let Some(v) = saved.take() {
                let _ = set_volume(v);
                self.on.store(false, Ordering::SeqCst);
            }
        }
        fn is_ducked(&self) -> bool {
            self.on.load(Ordering::SeqCst)
        }
    }

    impl Drop for MacDuck {
        fn drop(&mut self) {
            self.set_ducked(false);
        }
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

    #[test]
    fn duck_mic_only_when_capture_audio_is_on() {
        assert!(!should_duck_mic(false));
        assert!(should_duck_mic(true));
        let d = FakeDuck::new();
        assert!(!d.is_ducked());
        d.set_ducked(true);
        assert!(d.is_ducked());
        d.set_ducked(false);
        assert!(!d.is_ducked());
    }

    #[test]
    fn voice_player_argv_is_raw_s16le_on_stdin() {
        let ff = ffmpeg_audiotoolbox_argv(16_000);
        assert!(ff.windows(2).any(|w| w[0] == "-f" && w[1] == "s16le"));
        assert!(ff.windows(2).any(|w| w[0] == "-i" && w[1] == "pipe:0"));
        assert!(ff.windows(2).any(|w| w[0] == "-f" && w[1] == "audiotoolbox"));
        let play = ffplay_voice_argv(16_000);
        assert!(play.iter().any(|a| a == "-nodisp"));
        assert!(play.windows(2).any(|w| w[0] == "-f" && w[1] == "s16le"));
        assert!(play.windows(2).any(|w| w[0] == "-i" && w[1] == "pipe:0"));
        let cands = voice_player_candidates(16_000);
        assert!(cands.iter().any(|(bin, _)| *bin == "ffplay"));
    }

    #[test]
    fn pipe_voice_writes_continuous_pcm_to_child_stdin() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("out.raw");
        let file = fs::File::create(&path).unwrap();
        let v = PipeVoice::spawned("/bin/cat", &[], Stdio::from(file)).expect("cat");
        {
            let mut g = v.inner.lock();
            assert!(PipeVoice::write_pcm(&mut g, &[1, 2, 3, 4]));
            assert!(PipeVoice::write_pcm(&mut g, &[5, 6]));
        }
        drop(v);
        assert_eq!(fs::read(&path).unwrap(), vec![1, 2, 3, 4, 5, 6]);
    }
}
