//! Live JPEGs of inactive displays so the all-monitors map is a preview, not a still.

use crate::capture::{enumerate_devices, Device};
use crate::encoder::{sysname, MACOS_RECORDING_HUD_PX};
use crate::files::encode_b64;
use crate::frame::MjpegFramer;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

/// Preview fps for unselected monitors. Tiny 180px JPEGs; 15 fps feels live
/// without a second full-res 30 fps encode on every display.
pub const THUMB_FPS: u32 = 15;
pub const THUMB_WIDTH: u32 = 180;
const THUMB_Q: &str = "12";
const MEMBER_POLL: Duration = Duration::from_millis(400);
const FIRST_FRAME: Duration = Duration::from_secs(8);
const STALL: Duration = Duration::from_secs(3);

pub fn same_input(a: &str, b: &str) -> bool {
    a.trim_end_matches(':') == b.trim_end_matches(':')
}

pub fn inactive_devices(devices: &[Device], current: &str) -> Vec<Device> {
    devices
        .iter()
        .filter(|d| !same_input(&d.id, current))
        .cloned()
        .collect()
}

pub fn thumbs_json(items: &[(String, Vec<u8>)]) -> String {
    let items: Vec<Value> = items
        .iter()
        .filter(|(_, jpeg)| jpeg.starts_with(b"\xff\xd8"))
        .map(|(id, jpeg)| json!({"id": id, "data": encode_b64(jpeg)}))
        .collect();
    json!({"type": "thumbs", "items": items}).to_string()
}

/// Merge a live `{type:thumbs}` frame into the publisher snapshot so a
/// reconnecting watcher gets every inactive display, not only the last tile.
pub fn merge_thumbs_latest(latest: &mut Vec<String>, msg: &str) -> bool {
    let Ok(v) = serde_json::from_str::<Value>(msg) else {
        return false;
    };
    if v.get("type").and_then(|t| t.as_str()) != Some("thumbs") {
        return false;
    }
    let Some(new_items) = v.get("items").and_then(|i| i.as_array()) else {
        return false;
    };
    if let Some(existing) = latest.iter_mut().find(|m| m.contains("\"thumbs\"")) {
        if let Ok(cur) = serde_json::from_str::<Value>(existing) {
            let mut map: HashMap<String, Value> = HashMap::new();
            if let Some(arr) = cur.get("items").and_then(|i| i.as_array()) {
                for it in arr {
                    if let Some(id) = it.get("id").and_then(|x| x.as_str()) {
                        map.insert(id.to_string(), it.clone());
                    }
                }
            }
            for it in new_items {
                if let Some(id) = it.get("id").and_then(|x| x.as_str()) {
                    map.insert(id.to_string(), it.clone());
                }
            }
            let items: Vec<Value> = map.into_values().collect();
            *existing = json!({"type": "thumbs", "items": items}).to_string();
            return true;
        }
    }
    latest.push(msg.to_string());
    true
}

pub fn thumb_vf() -> String {
    let hud = MACOS_RECORDING_HUD_PX;
    format!("crop=iw:ih-{hud}:0:{hud},scale=min({THUMB_WIDTH}\\,iw):-2")
}

/// `oneshot` grabs a single JPEG (tests / fallback). Live omits `-frames:v`.
pub fn thumb_ffmpeg_args(input_id: &str, oneshot: bool) -> Vec<String> {
    let fps = if oneshot { 1 } else { THUMB_FPS };
    let mut argv = vec!["-hide_banner".into(), "-loglevel".into(), "error".into()];
    if !oneshot {
        argv.extend([
            "-fflags".into(),
            "+nobuffer".into(),
            "-flags".into(),
            "low_delay".into(),
        ]);
    }
    argv.extend([
        "-f".into(),
        "avfoundation".into(),
        "-framerate".into(),
        fps.to_string(),
        "-i".into(),
        input_id.into(),
        "-an".into(),
    ]);
    if oneshot {
        argv.extend(["-frames:v".into(), "1".into()]);
    }
    argv.extend([
        "-vf".into(),
        thumb_vf(),
        "-q:v".into(),
        THUMB_Q.into(),
        "-f".into(),
        "mjpeg".into(),
        "pipe:1".into(),
    ]);
    argv
}

pub async fn capture_jpeg(input_id: &str) -> Option<Vec<u8>> {
    if sysname() != "Darwin" || input_id.is_empty() {
        return None;
    }
    let args = thumb_ffmpeg_args(input_id, true);
    let mut child = Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let mut jpeg = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(4), stdout.read_to_end(&mut jpeg));
    let ok = matches!(read.await, Ok(Ok(_)) if jpeg.starts_with(b"\xff\xd8"));
    let _ = child.kill().await;
    let _ = child.wait().await;
    if ok {
        Some(jpeg)
    } else {
        None
    }
}

pub async fn grab_inactive(current: &str) -> Vec<(String, Vec<u8>)> {
    let devices = enumerate_devices();
    if devices.len() < 2 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for d in inactive_devices(&devices, current) {
        if let Some(jpeg) = capture_jpeg(&d.id).await {
            out.push((d.id, jpeg));
        }
    }
    out
}

enum StreamStop {
    Closed,
    Restart,
}

async fn run_stream(input_id: &str, tx: &mpsc::Sender<(String, Vec<u8>)>) -> StreamStop {
    let args = thumb_ffmpeg_args(input_id, false);
    let mut child = match Command::new("ffmpeg")
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return StreamStop::Restart,
    };
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill().await;
        return StreamStop::Restart;
    };
    let mut fr = MjpegFramer::new();
    let mut buf = [0u8; 16384];
    let mut got = false;
    let stop = 'read: loop {
        let wait = if got { STALL } else { FIRST_FRAME };
        let read = tokio::time::timeout(wait, stdout.read(&mut buf)).await;
        let n = match read {
            Ok(Ok(0)) | Err(_) | Ok(Err(_)) => break 'read StreamStop::Restart,
            Ok(Ok(n)) => n,
        };
        got = true;
        for jpeg in fr.push(&buf[..n]) {
            if !jpeg.starts_with(b"\xff\xd8") {
                continue;
            }
            match tx.try_send((input_id.to_string(), jpeg)) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {}
                Err(mpsc::error::TrySendError::Closed(_)) => break 'read StreamStop::Closed,
            }
        }
    };
    let _ = child.kill().await;
    let _ = child.wait().await;
    stop
}

async fn stream_device(input_id: String, tx: mpsc::Sender<(String, Vec<u8>)>) {
    if sysname() != "Darwin" || input_id.is_empty() {
        return;
    }
    loop {
        match run_stream(&input_id, &tx).await {
            StreamStop::Closed => return,
            StreamStop::Restart => {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

/// Keep one ffmpeg MJPEG process per inactive display and push each frame.
pub fn spawn_live<C, P>(get_current: C, push: P)
where
    C: Fn() -> String + Send + 'static,
    P: Fn(String) + Send + Sync + 'static,
{
    tokio::spawn(async move {
        run_live(get_current, Arc::new(push)).await;
    });
}

async fn run_live<C, P>(get_current: C, push: Arc<P>)
where
    C: Fn() -> String + Send + 'static,
    P: Fn(String) + Send + Sync + 'static,
{
    let (tx, mut rx) = mpsc::channel::<(String, Vec<u8>)>(2);
    let push_rx = push.clone();
    tokio::spawn(async move {
        while let Some((id, jpeg)) = rx.recv().await {
            push_rx(thumbs_json(&[(id, jpeg)]));
        }
    });
    let mut tasks: HashMap<String, tokio::task::JoinHandle<()>> = HashMap::new();
    loop {
        let current = get_current();
        let devices = enumerate_devices();
        let want: HashSet<String> = inactive_devices(&devices, &current)
            .into_iter()
            .map(|d| d.id)
            .collect();
        tasks.retain(|id, h| {
            if want.contains(id) {
                true
            } else {
                h.abort();
                false
            }
        });
        for id in &want {
            if tasks.contains_key(id) {
                continue;
            }
            let tx = tx.clone();
            let id2 = id.clone();
            tasks.insert(
                id.clone(),
                tokio::spawn(async move { stream_device(id2, tx).await }),
            );
        }
        tokio::time::sleep(MEMBER_POLL).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, main: bool) -> Device {
        Device {
            id: id.into(),
            name: id.into(),
            x: 0,
            y: 0,
            width: 100,
            height: 80,
            main,
        }
    }

    #[test]
    fn inactive_devices_skips_current_colon_variants() {
        let ds = vec![dev("2:", true), dev("3:", false)];
        let rest = inactive_devices(&ds, "2");
        assert_eq!(rest.len(), 1);
        assert_eq!(rest[0].id, "3:");
        assert!(inactive_devices(&ds, "3:").iter().all(|d| d.id == "2:"));
        assert!(inactive_devices(&ds, "2:").iter().all(|d| d.id == "3:"));
    }

    #[test]
    fn thumbs_json_drops_non_jpeg_and_encodes_ids() {
        let jpeg = b"\xff\xd8\xff\xd9".to_vec();
        let msg = thumbs_json(&[("2:".into(), jpeg), ("3:".into(), b"nope".to_vec())]);
        assert!(msg.contains("\"thumbs\""));
        assert!(msg.contains("2:"));
        assert!(!msg.contains("nope"));
        assert!(msg.contains(&crate::files::encode_b64(b"\xff\xd8\xff\xd9")));
    }

    #[test]
    fn live_thumb_args_are_continuous_mjpeg() {
        let joined = thumb_ffmpeg_args("2:", false).join(" ");
        assert!(joined.contains(&format!("-framerate {THUMB_FPS}")));
        assert!(!joined.contains("-frames:v"));
        assert!(joined.contains("mjpeg"));
        assert!(joined.contains("2:"));
        assert!(joined.contains("low_delay"));
        assert!(
            THUMB_FPS >= 10,
            "inactive map tiles must feel live, not ~3s stills"
        );
    }

    #[test]
    fn oneshot_thumb_args_grab_one_frame() {
        let joined = thumb_ffmpeg_args("3:", true).join(" ");
        assert!(joined.contains("-frames:v 1"));
        assert!(joined.contains("-framerate 1"));
    }

    #[test]
    fn merge_thumbs_latest_unions_display_ids() {
        let a = b"\xff\xd8\xff\xd9".to_vec();
        let mut latest = Vec::new();
        assert!(merge_thumbs_latest(
            &mut latest,
            &thumbs_json(&[("2:".into(), a.clone())])
        ));
        assert!(merge_thumbs_latest(
            &mut latest,
            &thumbs_json(&[("3:".into(), a)])
        ));
        assert_eq!(latest.len(), 1);
        assert!(latest[0].contains("2:"));
        assert!(latest[0].contains("3:"));
        assert!(!merge_thumbs_latest(
            &mut latest,
            &json!({"type": "flags"}).to_string()
        ));
    }
}
