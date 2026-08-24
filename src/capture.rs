//! ffmpeg subprocess + stdout framing into the hub.

use crate::config::Config;
use crate::encoder::{build_ffmpeg_argv, have_videotoolbox, sysname};
use crate::frame::{jpeg_size, MjpegFramer, Mp4Framer, Unit};
use crate::hub::Hub;
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

const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(12);
const STALL_TIMEOUT: Duration = Duration::from_secs(4);

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
                }
                hub.clear();
                match run_ffmpeg(cfg.clone(), input.clone(), hub.clone(), status.clone()).await {
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
                tokio::time::sleep(backoff).await;
                backoff = capture_backoff(backoff);
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
}

pub fn enumerate_devices() -> Vec<Device> {
    if sysname() != "Darwin" {
        let mut out = vec![Device {
            id: "desktop".into(),
            name: "Desktop".into(),
        }];
        if sysname() == "Linux" {
            out.push(Device {
                id: ":0.0".into(),
                name: "X11 :0.0".into(),
            });
        }
        return out;
    }
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
    let Ok(out) = out else {
        return vec![];
    };
    let stderr = String::from_utf8_lossy(&out.stderr);
    let re = Regex::new(r"\[(\d+)\] (Capture screen \d+)").unwrap();
    re.captures_iter(&stderr)
        .map(|c| Device {
            id: format!("{}:", &c[1]),
            name: c[2].to_string(),
        })
        .collect()
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
}
