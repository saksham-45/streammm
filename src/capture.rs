//! ffmpeg subprocess + stdout framing into the hub.

use crate::config::Config;
use crate::encoder::{build_ffmpeg_argv, have_videotoolbox, sysname};
use crate::frame::{jpeg_size, MjpegFramer, Mp4Framer, Unit};
use crate::hub::Hub;
use crate::input::{list_cg_displays, DisplayInfo};
use crate::protocol::{TYPE_FRAG, TYPE_JPEG};
use bytes::Bytes;
use parking_lot::Mutex;
use regex::Regex;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub fn capture_backoff(prev: Duration) -> Duration {
    prev.saturating_mul(2).clamp(Duration::from_secs(1), Duration::from_secs(8))
}

/// After a capture that already produced frames (app switch / brief stall),
/// restart immediately. Cold failures still back off.
pub fn next_capture_backoff(prev: Duration, had_media: bool) -> Duration {
    if had_media {
        Duration::from_secs(1)
    } else {
        capture_backoff(prev)
    }
}

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(12);
const STALL_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone, Debug, Default)]
pub struct CaptureStatus {
    pub running: bool,
    pub error: String,
    pub width: u32,
    pub height: u32,
    pub input: String,
    pub frames_per_fragment: u32,
}

#[derive(Clone)]
pub struct Capture {
    hub: Hub,
    status: Arc<Mutex<CaptureStatus>>,
    task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
}

impl Capture {
    pub fn new(hub: Hub) -> Self {
        Self {
            hub,
            status: Arc::new(Mutex::new(CaptureStatus::default())),
            task: Arc::new(Mutex::new(None)),
        }
    }

    pub fn status(&self) -> CaptureStatus {
        self.status.lock().clone()
    }

    pub fn stop(&self) {
        if let Some(h) = self.task.lock().take() {
            h.abort();
        }
        self.status.lock().running = false;
    }

    pub fn start(&self, cfg: Config) {
        self.stop();
        let input = resolve_input(&cfg.capture.input);
        {
            let mut st = self.status.lock();
            st.input = input.clone();
            st.error.clear();
            st.running = true;
            st.frames_per_fragment = if cfg.encoder.mode == "mjpeg" {
                1
            } else {
                cfg.encoder.gop_frames
            };
            st.width = 0;
            st.height = 0;
        }
        let hub = self.hub.clone();
        let status = self.status.clone();
        let handle = tokio::spawn(async move {
            let mut backoff = Duration::from_secs(1);
            loop {
                {
                    let mut st = status.lock();
                    st.running = true;
                    st.width = 0;
                    st.height = 0;
                }
                hub.clear();
                let result = run_ffmpeg(cfg.clone(), input.clone(), hub.clone(), status.clone()).await;
                let had_media = status.lock().width > 0;
                match result {
                    Ok(()) => {
                        tracing::warn!("capture: ffmpeg exited; restarting");
                        status.lock().error = "ffmpeg exited; restarting".into();
                    }
                    Err(e) => {
                        tracing::warn!("capture: {e}; restarting");
                        status.lock().error = format!("{e}; restarting");
                    }
                }
                status.lock().running = false;
                backoff = next_capture_backoff(backoff, had_media);
                tokio::time::sleep(backoff).await;
            }
        });
        *self.task.lock() = Some(handle);
    }

    pub fn restart(&self, cfg: Config) {
        self.hub.clear();
        self.start(cfg);
    }
}

pub fn resolve_input(raw: &str) -> String {
    if !raw.is_empty() {
        return raw.to_string();
    }
    default_input()
}

pub fn default_input() -> String {
    match sysname() {
        "Darwin" => enumerate_devices()
            .into_iter()
            .next()
            .map(|d| d.id)
            .unwrap_or_else(|| "3:".into()),
        "Windows" => "desktop".into(),
        _ => std::env::var("DISPLAY").unwrap_or_else(|_| ":0.0".into()),
    }
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub x: i32,
    #[serde(default)]
    pub y: i32,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default)]
    pub main: bool,
}

impl From<DisplayInfo> for Device {
    fn from(d: DisplayInfo) -> Self {
        Self {
            id: d.id,
            name: d.name,
            x: d.x,
            y: d.y,
            width: d.width,
            height: d.height,
            main: d.main,
        }
    }
}

impl Device {
    pub fn as_info(&self) -> DisplayInfo {
        DisplayInfo {
            id: self.id.clone(),
            name: self.name.clone(),
            x: self.x,
            y: self.y,
            width: self.width,
            height: self.height,
            main: self.main,
        }
    }
}

pub fn enumerate_devices() -> Vec<Device> {
    let cg = list_cg_displays();
    if sysname() != "Darwin" {
        let mut out = vec![Device {
            id: "desktop".into(),
            name: "Desktop".into(),
            main: true,
            ..Device::from(DisplayInfo::default())
        }];
        if let Some(d) = cg.first() {
            out[0].x = d.x;
            out[0].y = d.y;
            out[0].width = d.width;
            out[0].height = d.height;
        }
        if sysname() == "Linux" {
            out.push(Device {
                id: ":0.0".into(),
                name: "X11 :0.0".into(),
                ..Device::from(DisplayInfo::default())
            });
        }
        return out;
    }
    let ff = ffmpeg_avfoundation_screens();
    if ff.is_empty() {
        return cg.into_iter().map(Device::from).collect();
    }
    ff.into_iter()
        .map(|(dev_id, screen_idx, label)| device_from_ffmpeg_screen(&dev_id, screen_idx, &label, &cg))
        .collect()
}

/// Pair an AVFoundation `Capture screen N` entry with CG bounds at the same index.
pub fn device_from_ffmpeg_screen(
    dev_id: &str,
    screen_idx: usize,
    label: &str,
    cg: &[DisplayInfo],
) -> Device {
    let bounds = crate::input::display_for_ffmpeg_screen(screen_idx, cg);
    let (x, y, width, height, main) = match bounds {
        Some(b) => (b.x, b.y, b.width, b.height, b.main),
        None => (0, 0, 0, 0, screen_idx == 0),
    };
    let name = if width > 0 {
        format!("{label} — {width}×{height}")
    } else {
        label.to_string()
    };
    Device {
        id: format!("{dev_id}:"),
        name,
        x,
        y,
        width,
        height,
        main,
    }
}

fn ffmpeg_avfoundation_list_stderr() -> String {
    let out = std::process::Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-f",
            "avfoundation",
            "-list_devices",
            "true",
            "-i",
            "",
        ])
        .output();
    match out {
        Ok(o) => String::from_utf8_lossy(&o.stderr).into_owned(),
        Err(_) => String::new(),
    }
}

fn ffmpeg_avfoundation_screens() -> Vec<(String, usize, String)> {
    let stderr = ffmpeg_avfoundation_list_stderr();
    let re = Regex::new(r"\[(\d+)\] (Capture screen (\d+))").unwrap();
    re.captures_iter(&stderr)
        .filter_map(|c| {
            let dev = c.get(1)?.as_str().to_string();
            let label = c.get(2)?.as_str().to_string();
            let idx = c.get(3)?.as_str().parse::<usize>().ok()?;
            Some((dev, idx, label))
        })
        .collect()
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// Parse ffmpeg `-list_devices` audio section (mics, BlackHole, Loopback).
pub fn parse_avfoundation_audio(stderr: &str) -> Vec<AudioDevice> {
    let lower = stderr.to_ascii_lowercase();
    let Some(idx) = lower.find("audio devices:") else {
        return Vec::new();
    };
    let rest = &stderr[idx..];
    let re = Regex::new(r"\[(\d+)\] (.+)").unwrap();
    rest.lines()
        .filter_map(|line| {
            let c = re.captures(line)?;
            let id = c.get(1)?.as_str().to_string();
            let name = c.get(2)?.as_str().trim().to_string();
            if name.is_empty() || name.to_ascii_lowercase().contains("audio devices:") {
                return None;
            }
            Some(AudioDevice { id, name })
        })
        .collect()
}

pub fn enumerate_audio_devices() -> Vec<AudioDevice> {
    if sysname() != "Darwin" {
        return Vec::new();
    }
    parse_avfoundation_audio(&ffmpeg_avfoundation_list_stderr())
}

async fn run_ffmpeg(
    cfg: Config,
    input: String,
    hub: Hub,
    status: Arc<Mutex<CaptureStatus>>,
) -> anyhow::Result<()> {
    let argv = build_ffmpeg_argv(&cfg, &input, sysname(), have_videotoolbox());
    tracing::info!("ffmpeg {}", argv.join(" "));
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            anyhow::anyhow!("ffmpeg not found on PATH")
        } else {
            anyhow::anyhow!("failed to start ffmpeg: {e}")
        }
    })?;
    let mut stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("no stdout"))?;
    let stderr = child.stderr.take();
    if let Some(mut err) = stderr {
        let status = status.clone();
        tokio::spawn(async move {
            let mut buf = Vec::new();
            let mut tmp = [0u8; 4096];
            let re = Regex::new(r"Stream #0:0[^\n]*? (\d{2,5})x(\d{2,5})").ok();
            loop {
                match err.read(&mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        buf.extend_from_slice(&tmp[..n]);
                        if buf.len() > 64 * 1024 {
                            buf.drain(..buf.len() - 4096);
                        }
                        let text = String::from_utf8_lossy(&buf);
                        if let Some(re) = &re {
                            if let Some(c) = re.captures(&text) {
                                let mut st = status.lock();
                                st.width = c[1].parse().unwrap_or(0);
                                st.height = c[2].parse().unwrap_or(0);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    let mut buf = [0u8; 65536];
    let mut got_data = false;
    if cfg.encoder.mode == "mjpeg" {
        let mut fr = MjpegFramer::new();
        loop {
            let n = read_stdout(&mut stdout, &mut buf, got_data).await?;
            if n == 0 {
                break;
            }
            got_data = true;
            for jpeg in fr.push(&buf[..n]) {
                let (w, h) = jpeg_size(&jpeg);
                {
                    let mut st = status.lock();
                    st.width = w;
                    st.height = h;
                    st.error.clear();
                }
                hub.publish_unit(TYPE_JPEG, Bytes::from(jpeg), w, h);
            }
        }
    } else {
        let mut fr = Mp4Framer::new();
        loop {
            let n = read_stdout(&mut stdout, &mut buf, got_data).await?;
            if n == 0 {
                break;
            }
            got_data = true;
            for unit in fr.push(&buf[..n]) {
                match unit {
                    Unit::Init(data) => {
                        let (w, h) = {
                            let mut st = status.lock();
                            st.error.clear();
                            (st.width, st.height)
                        };
                        hub.publish_init(Bytes::from(data), w, h);
                    }
                    Unit::Fragment(data) => {
                        hub.publish_unit(TYPE_FRAG, Bytes::from(data), 0, 0);
                    }
                    Unit::Jpeg(_) => {}
                }
            }
        }
    }
    let code = child.wait().await?;
    if !code.success() {
        anyhow::bail!("ffmpeg exited with code {:?}", code.code());
    }
    Ok(())
}

async fn read_stdout<R: tokio::io::AsyncRead + Unpin>(
    stdout: &mut R,
    buf: &mut [u8],
    got_data: bool,
) -> anyhow::Result<usize> {
    let wait = if got_data {
        STALL_TIMEOUT
    } else {
        FIRST_FRAME_TIMEOUT
    };
    match tokio::time::timeout(wait, stdout.read(buf)).await {
        Ok(Ok(n)) => Ok(n),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => anyhow::bail!("ffmpeg stalled (no output for {wait:?})"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_backoff_caps_at_8s() {
        let mut d = Duration::from_secs(1);
        d = capture_backoff(d);
        assert_eq!(d, Duration::from_secs(2));
        for _ in 0..10 {
            d = capture_backoff(d);
        }
        assert_eq!(d, Duration::from_secs(8));
    }

    #[test]
    fn stall_timeout_survives_app_switch() {
        assert!(
            STALL_TIMEOUT >= Duration::from_secs(15),
            "app switches stall avfoundation for several seconds; 4s restarts kill the stream: {STALL_TIMEOUT:?}"
        );
    }

    #[test]
    fn restart_backoff_resets_after_successful_capture() {
        let after_stall = next_capture_backoff(Duration::from_secs(8), true);
        assert_eq!(after_stall, Duration::from_secs(1));
        let cold = next_capture_backoff(Duration::from_secs(2), false);
        assert_eq!(cold, Duration::from_secs(4));
    }

    #[test]
    fn ffmpeg_screen_1_gets_second_display_bounds_not_main() {
        let cg = vec![
            DisplayInfo {
                id: "cg0".into(),
                name: "main".into(),
                x: 0,
                y: 0,
                width: 1440,
                height: 900,
                main: true,
            },
            DisplayInfo {
                id: "cg1".into(),
                name: "ext".into(),
                x: 1440,
                y: -200,
                width: 2560,
                height: 1440,
                main: false,
            },
        ];
        let d0 = device_from_ffmpeg_screen("2", 0, "Capture screen 0", &cg);
        let d1 = device_from_ffmpeg_screen("3", 1, "Capture screen 1", &cg);
        assert_eq!(d0.id, "2:");
        assert!(d0.main);
        assert_eq!((d0.x, d0.y, d0.width, d0.height), (0, 0, 1440, 900));
        assert_eq!(d1.id, "3:");
        assert!(!d1.main);
        assert_eq!((d1.x, d1.y, d1.width, d1.height), (1440, -200, 2560, 1440));
        assert!(d1.name.contains("2560×1440"));
    }

    #[test]
    fn avfoundation_screen_regex_captures_index() {
        let re = Regex::new(r"\[(\d+)\] (Capture screen (\d+))").unwrap();
        let sample = "[AVFoundation] [3] Capture screen 0\n[AVFoundation] [4] Capture screen 1";
        let got: Vec<(String, usize)> = re
            .captures_iter(sample)
            .map(|c| (c[1].to_string(), c[3].parse().unwrap()))
            .collect();
        assert_eq!(got, vec![("3".into(), 0usize), ("4".into(), 1usize)]);
    }

    #[test]
    fn parse_avfoundation_audio_lists_mic_and_loopback() {
        let sample = "\
AVFoundation video devices:\n\
[0] FaceTime HD Camera\n\
[1] Capture screen 0\n\
AVFoundation audio devices:\n\
[0] MacBook Pro Microphone\n\
[1] BlackHole 2ch\n\
[2] ZoomAudioDevice\n";
        let got = parse_avfoundation_audio(sample);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].id, "0");
        assert_eq!(got[0].name, "MacBook Pro Microphone");
        assert_eq!(got[1].id, "1");
        assert!(got[1].name.contains("BlackHole"));
        assert!(parse_avfoundation_audio("video only").is_empty());
    }

    #[test]
    fn enumerate_devices_returns_named_screens() {
        let ds = enumerate_devices();
        assert!(!ds.is_empty(), "must list at least one capture target");
        assert!(ds.iter().any(|d| !d.id.is_empty() && !d.name.is_empty()));
    }
}
