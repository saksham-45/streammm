//! Config JSON: load/save/clamp. Unknown keys are ignored.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

fn clamp<T: PartialOrd + Copy>(v: T, lo: T, hi: T) -> T {
    if v < lo {
        lo
    } else if v > hi {
        hi
    } else {
        v
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CaptureConfig {
    #[serde(default = "default_driver")]
    pub driver: String,
    #[serde(default)]
    pub input: String,
    #[serde(default = "default_fps")]
    pub fps: u32,
    #[serde(default = "default_scale")]
    pub scale: f64,
    #[serde(default = "default_jpeg")]
    pub jpeg_quality: u32,
    /// macOS avfoundation microphone into the live fMP4 (H.264/HEVC only).
    #[serde(default)]
    pub audio: bool,
    /// avfoundation audio device index (`0` = default mic; BlackHole/Loopback for system audio).
    #[serde(default = "default_audio_input")]
    pub audio_input: String,
}

fn default_driver() -> String {
    "ffmpeg".into()
}
fn default_fps() -> u32 {
    30
}
fn default_scale() -> f64 {
    1.0
}
fn default_jpeg() -> u32 {
    95
}
fn default_audio_input() -> String {
    "0".into()
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            driver: default_driver(),
            input: String::new(),
            fps: default_fps(),
            scale: default_scale(),
            jpeg_quality: default_jpeg(),
            audio: false,
            audio_input: default_audio_input(),
        }
    }
}

impl CaptureConfig {
    pub fn clamp(&mut self) {
        self.fps = clamp(self.fps, 1, 60);
        self.scale = clamp(self.scale, 0.25, 1.0);
        self.jpeg_quality = clamp(self.jpeg_quality, 30, 95);
        self.audio_input = clamp_audio_input(&self.audio_input);
    }
}

pub fn clamp_audio_input(raw: &str) -> String {
    let s = raw.trim().trim_end_matches(':');
    if !s.is_empty() && s.len() <= 4 && s.chars().all(|c| c.is_ascii_digit()) {
        s.to_string()
    } else {
        default_audio_input()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EncoderConfig {
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default = "default_bitrate")]
    pub bitrate_kbps: u32,
    #[serde(default = "default_gop")]
    pub gop_frames: u32,
    #[serde(default = "default_max_w")]
    pub max_width: u32,
    #[serde(default = "default_max_h")]
    pub max_height: u32,
}

fn default_mode() -> String {
    "ffmpeg".into()
}
fn default_bitrate() -> u32 {
    20000
}
fn default_gop() -> u32 {
    15
}
fn default_max_w() -> u32 {
    3840
}
fn default_max_h() -> u32 {
    4320
}

impl Default for EncoderConfig {
    fn default() -> Self {
        Self {
            mode: default_mode(),
            bitrate_kbps: default_bitrate(),
            gop_frames: default_gop(),
            max_width: default_max_w(),
            max_height: default_max_h(),
        }
    }
}

impl EncoderConfig {
    pub fn clamp(&mut self) {
        if self.mode != "mjpeg" && self.mode != "ffmpeg" && self.mode != "hevc" {
            self.mode = "mjpeg".into();
        }
        self.bitrate_kbps = clamp(self.bitrate_kbps, 2000, 50000);
        self.gop_frames = clamp(self.gop_frames, 6, 60);
        self.max_width = clamp(self.max_width, 640, 7680);
        self.max_height = clamp(self.max_height, 360, 4320);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_llm_url")]
    pub base_url: String,
    #[serde(default = "default_llm_key")]
    pub api_key: String,
    #[serde(default = "default_llm_model")]
    pub model: String,
    #[serde(default = "default_llm_interval")]
    pub interval_sec: u32,
    #[serde(default)]
    pub prompt: String,
}

fn default_llm_url() -> String {
    "http://127.0.0.1:11434/v1".into()
}
fn default_llm_key() -> String {
    "ollama".into()
}
fn default_llm_model() -> String {
    "llama3.2-vision".into()
}
fn default_llm_interval() -> u32 {
    5
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            base_url: default_llm_url(),
            api_key: default_llm_key(),
            model: default_llm_model(),
            interval_sec: default_llm_interval(),
            prompt: String::new(),
        }
    }
}

impl LlmConfig {
    pub fn clamp(&mut self) {
        self.interval_sec = clamp(self.interval_sec, 2, 3600);
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct CloudflareConfig {
    #[serde(default)]
    pub publish_url: String,
    #[serde(default)]
    pub watch_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct ControlConfig {
    /// Remote mouse/keyboard from OTP viewers. OFF by default.
    #[serde(default)]
    pub enabled: bool,
    /// AI computer-use from OTP viewers. OFF by default.
    #[serde(default)]
    pub ai_enabled: bool,
    /// Swallow host HID while a remote controller is driving. ⌘⇧Esc unlocks.
    #[serde(default)]
    pub block_local: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct AccessConfig {
    /// Persistent password join after the 6-digit PIN expires. OFF by default.
    #[serde(default)]
    pub unattended: bool,
    /// SHA-256 hex of the unattended password. Never the plaintext.
    #[serde(default)]
    pub password_hash: String,
}

impl AccessConfig {
    pub fn clamp(&mut self) {
        self.password_hash = normalize_password_hash(&self.password_hash);
        if !self.unattended {
            self.password_hash.clear();
        }
    }
}

pub fn normalize_password_hash(raw: &str) -> String {
    let s = raw.trim().to_ascii_lowercase();
    if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        s
    } else {
        String::new()
    }
}

/// Pull plaintext `access.password` out of a config PATCH so it is never stored.
pub fn take_access_password(patch: &mut serde_json::Value) -> Option<String> {
    let obj = patch.get_mut("access")?.as_object_mut()?;
    match obj.remove("password") {
        Some(serde_json::Value::String(s)) => Some(s),
        _ => None,
    }
}

pub fn apply_unattended_password(cfg: &mut Config, password: Option<String>) -> Result<(), &'static str> {
    if !cfg.access.unattended {
        cfg.access.password_hash.clear();
        return Ok(());
    }
    let Some(raw) = password else {
        return Ok(());
    };
    let t = raw.trim();
    if t.is_empty() {
        return Ok(());
    }
    let n = t.chars().count();
    if n < 8 || n > 128 {
        return Err("unattended password must be 8–128 characters");
    }
    cfg.access.password_hash = crate::otp::sha256_hex(t);
    Ok(())
}

/// Host GET /api/config: never send the hash, only whether a password is set.
pub fn redact_access(cfg: &Config) -> serde_json::Value {
    let mut v = serde_json::to_value(cfg).unwrap_or(serde_json::json!({}));
    if let Some(access) = v.get_mut("access").and_then(|x| x.as_object_mut()) {
        let set = cfg.access.password_hash.len() == 64;
        access.remove("password_hash");
        access.insert("password_set".into(), serde_json::json!(set));
    }
    v
}

impl CloudflareConfig {
    pub fn clamp(&mut self) {
        self.publish_url = self.publish_url.trim().to_string();
        self.watch_url = self.watch_url.trim().to_string();
    }
}

/// True when the publisher must drop/reconnect (URL cleared or replaced).
pub fn cloudflare_endpoint_changed(old: &Config, new: &Config) -> bool {
    old.cloudflare.publish_url != new.cloudflare.publish_url
        || old.cloudflare.watch_url != new.cloudflare.watch_url
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub capture: CaptureConfig,
    #[serde(default)]
    pub encoder: EncoderConfig,
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default)]
    pub cloudflare: CloudflareConfig,
    #[serde(default)]
    pub control: ControlConfig,
    #[serde(default)]
    pub access: AccessConfig,
}

fn default_host() -> String {
    "127.0.0.1".into()
}
fn default_port() -> u16 {
    8080
}

impl Default for Config {
    fn default() -> Self {
        Self {
            host: default_host(),
            port: default_port(),
            token: String::new(),
            capture: CaptureConfig::default(),
            encoder: EncoderConfig::default(),
            llm: LlmConfig::default(),
            cloudflare: CloudflareConfig::default(),
            control: ControlConfig::default(),
            access: AccessConfig::default(),
        }
    }
}

impl Config {
    pub fn clamp(&mut self) {
        if self.port == 0 {
            self.port = 8080;
        }
        self.capture.clamp();
        self.encoder.clamp();
        self.llm.clamp();
        self.cloudflare.clamp();
        self.access.clamp();
    }

    pub fn from_value(v: serde_json::Value) -> Self {
        let mut cfg: Config = serde_json::from_value(v).unwrap_or_default();
        cfg.clamp();
        cfg
    }

    pub fn merge_patch(&self, patch: serde_json::Value) -> Self {
        let mut cur = serde_json::to_value(self).unwrap_or(serde_json::json!({}));
        merge_json(&mut cur, patch);
        Self::from_value(cur)
    }
}

fn merge_json(base: &mut serde_json::Value, patch: serde_json::Value) {
    match (base, patch) {
        (serde_json::Value::Object(base_map), serde_json::Value::Object(patch_map)) => {
            for (k, v) in patch_map {
                if let Some(existing) = base_map.get_mut(&k) {
                    if existing.is_object() && v.is_object() {
                        merge_json(existing, v);
                    } else {
                        *existing = v;
                    }
                } else {
                    base_map.insert(k, v);
                }
            }
        }
        (base, patch) => *base = patch,
    }
}

pub fn load(path: &Path) -> anyhow::Result<Config> {
    if !path.exists() {
        let cfg = Config::default();
        save(&cfg, path)?;
        return Ok(cfg);
    }
    let text = fs::read_to_string(path)?;
    let value: serde_json::Value = serde_json::from_str(&text)?;
    Ok(Config::from_value(value))
}

pub fn save(cfg: &Config, path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let text = serde_json::to_string_pretty(cfg)? + "\n";
    fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_are_twitch_class_h264() {
        let cfg = Config::default();
        assert_eq!(cfg.host, "127.0.0.1");
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.token, "");
        assert_eq!(cfg.capture.driver, "ffmpeg");
        assert_eq!(cfg.capture.fps, 30);
        assert_eq!(cfg.capture.scale, 1.0);
        assert_eq!(cfg.capture.jpeg_quality, 95);
        assert!(!cfg.capture.audio, "mic audio is off until the host enables it");
        assert_eq!(cfg.capture.audio_input, "0");
        assert_eq!(cfg.encoder.mode, "ffmpeg");
        assert_eq!(cfg.encoder.bitrate_kbps, 20000);
        assert_eq!(cfg.encoder.gop_frames, 15);
        assert_eq!(cfg.encoder.max_width, 3840);
        assert_eq!(cfg.encoder.max_height, 4320);
        assert_eq!(cfg.cloudflare.publish_url, "");
        assert!(!cfg.llm.enabled);
        assert!(!cfg.access.unattended);
        assert!(cfg.access.password_hash.is_empty());
    }

    #[test]
    fn missing_file_writes_defaults() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.encoder.mode, "ffmpeg");
        assert_eq!(cfg.encoder.bitrate_kbps, 20000);
        assert!(path.exists());
    }

    #[test]
    fn clamps_low_and_high_bounds() {
        let low = Config::from_value(serde_json::json!({
            "capture": {"fps": 0, "scale": 0.1, "jpeg_quality": 10, "audio_input": "../mic"},
            "encoder": {"bitrate_kbps": 100, "gop_frames": 1},
            "llm": {"interval_sec": 1}
        }));
        assert_eq!(low.capture.audio_input, "0");
        assert_eq!(low.capture.fps, 1);
        assert_eq!(low.capture.scale, 0.25);
        assert_eq!(low.capture.jpeg_quality, 30);
        assert_eq!(low.encoder.bitrate_kbps, 2000);
        assert_eq!(low.encoder.gop_frames, 6);
        assert_eq!(low.llm.interval_sec, 2);

        let high = Config::from_value(serde_json::json!({
            "capture": {"fps": 999, "scale": 3.0, "jpeg_quality": 100},
            "encoder": {"bitrate_kbps": 99999, "gop_frames": 999, "max_width": 8000},
            "llm": {"interval_sec": 99999}
        }));
        assert_eq!(high.capture.fps, 60);
        assert_eq!(high.capture.scale, 1.0);
        assert_eq!(high.capture.jpeg_quality, 95);
        assert_eq!(high.encoder.bitrate_kbps, 50000);
        assert_eq!(high.encoder.gop_frames, 60);
        assert_eq!(high.encoder.max_width, 7680);
        assert_eq!(high.llm.interval_sec, 3600);
    }

    #[test]
    fn invalid_mode_resets_to_mjpeg() {
        let cfg = Config::from_value(serde_json::json!({"encoder": {"mode": "webp"}}));
        assert_eq!(cfg.encoder.mode, "mjpeg");
        let cfg = Config::from_value(serde_json::json!({"encoder": {"mode": "hevc"}}));
        assert_eq!(cfg.encoder.mode, "hevc");
        let cfg = Config::from_value(serde_json::json!({"encoder": {"mode": "ffmpeg"}}));
        assert_eq!(cfg.encoder.mode, "ffmpeg");
    }

    #[test]
    fn unknown_keys_ignored_and_roundtrip() {
        let cfg = Config::from_value(serde_json::json!({
            "bogus": 1,
            "capture": {"nope": 2, "fps": 24},
            "encoder": {"weird": true, "mode": "ffmpeg", "gop_frames": 12},
            "cloudflare": {"publish_url": "wss://stream.example.com/publish", "watch_url": "wss://stream.example.com/watch"},
            "llm": {"x": "y", "model": "m"}
        }));
        assert_eq!(cfg.capture.fps, 24);
        assert_eq!(cfg.encoder.gop_frames, 12);
        assert_eq!(cfg.llm.model, "m");
        assert_eq!(cfg.port, 8080);
        assert_eq!(cfg.cloudflare.publish_url, "wss://stream.example.com/publish");
        let cfg2 = Config::from_value(serde_json::to_value(&cfg).unwrap());
        assert_eq!(cfg, cfg2);
    }

    #[test]
    fn clearing_publish_url_is_an_endpoint_change() {
        let mut old = Config::default();
        old.cloudflare.publish_url = "wss://edge.example/publish".into();
        let new = Config::default();
        assert!(cloudflare_endpoint_changed(&old, &new));
        assert!(!cloudflare_endpoint_changed(&new, &new));
    }

    #[test]
    fn unattended_password_is_hashed_and_redacted() {
        let mut cfg = Config::default();
        cfg.access.unattended = true;
        apply_unattended_password(&mut cfg, Some("short".into())).unwrap_err();
        apply_unattended_password(&mut cfg, Some("s3cret!!".into())).unwrap();
        assert_eq!(cfg.access.password_hash.len(), 64);
        let hash = cfg.access.password_hash.clone();
        apply_unattended_password(&mut cfg, Some("".into())).unwrap();
        assert_eq!(cfg.access.password_hash, hash, "blank password must keep the hash");
        let v = redact_access(&cfg);
        assert_eq!(v["access"]["unattended"], true);
        assert_eq!(v["access"]["password_set"], true);
        assert!(v["access"].get("password_hash").is_none());
        cfg.access.unattended = false;
        apply_unattended_password(&mut cfg, None).unwrap();
        assert!(cfg.access.password_hash.is_empty());
        let mut patch = serde_json::json!({"access": {"unattended": true, "password": "leave-me-out"}});
        assert_eq!(take_access_password(&mut patch).as_deref(), Some("leave-me-out"));
        assert!(patch["access"].get("password").is_none());
    }

    #[test]
    fn save_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = Config {
            token: "t".into(),
            capture: CaptureConfig {
                fps: 20,
                ..CaptureConfig::default()
            },
            ..Config::default()
        };
        save(&cfg, &path).unwrap();
        let cfg2 = load(&path).unwrap();
        assert_eq!(cfg2.capture.fps, 20);
        assert_eq!(cfg2.token, "t");
    }
}
