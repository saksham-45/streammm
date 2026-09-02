//! Tiny JPEGs of inactive displays so the all-monitors map is live, not last-frame.

use crate::capture::{enumerate_devices, Device};
use crate::encoder::{sysname, MACOS_RECORDING_HUD_PX};
use crate::files::encode_b64;
use serde_json::{json, Value};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

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

pub async fn capture_jpeg(input_id: &str) -> Option<Vec<u8>> {
    if sysname() != "Darwin" || input_id.is_empty() {
        return None;
    }
    let hud = MACOS_RECORDING_HUD_PX;
    let vf = format!("crop=iw:ih-{hud}:0:{hud},scale=min(180\\,iw):-2");
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "avfoundation",
            "-framerate",
            "1",
            "-i",
            input_id,
            "-frames:v",
            "1",
            "-vf",
            &vf,
            "-q:v",
            "12",
            "-f",
            "mjpeg",
            "pipe:1",
        ])
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
}
