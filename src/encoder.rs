//! ffmpeg argv: native-res cap (never upscale), 0.5 s VBV, no B-frames, VT or libx264 zerolatency.

use crate::config::Config;

/// Pre-encode filter: fps throttle, lanczos downscale, never upscale past max WxH, even dims.
pub fn scale_filter(cfg: &Config, fps: u32) -> String {
    let s = cfg.capture.scale;
    let mw = cfg.encoder.max_width;
    let mh = cfg.encoder.max_height;
    let (wexpr, hexpr) = if (s - 1.0).abs() < 1e-9 {
        (format!("min({mw}\\,iw)"), format!("min({mh}\\,ih)"))
    } else {
        (format!("min({mw}\\,iw*{s})"), format!("min({mh}\\,ih*{s})"))
    };
    format!(
        "setpts=PTS-STARTPTS,fps={fps},scale={wexpr}:{hexpr}:force_original_aspect_ratio=decrease:flags=lanczos,scale=trunc(iw/2)*2:trunc(ih/2)*2"
    )
}

fn input_part(sysname: &str, input_id: &str, fps: u32) -> Vec<String> {
    match sysname {
        "Darwin" => vec![
            "-f".into(),
            "avfoundation".into(),
            "-framerate".into(),
            fps.to_string(),
            "-i".into(),
            input_id.into(),
        ],
        "Windows" => vec![
            "-f".into(),
            "gdigrab".into(),
            "-framerate".into(),
            fps.to_string(),
            "-i".into(),
            "desktop".into(),
        ],
        _ => vec![
            "-f".into(),
            "x11grab".into(),
            "-framerate".into(),
            fps.to_string(),
            "-i".into(),
            input_id.into(),
        ],
    }
}

fn jpeg_q(quality: u32) -> u32 {
    let q = 2.0 + (95.0 - quality as f64) * 29.0 / 65.0;
    q.round().clamp(2.0, 31.0) as u32
}

/// Build the ffmpeg command. `sysname` is `"Darwin"` / `"Windows"` / `"Linux"`.
pub fn build_ffmpeg_argv(
    cfg: &Config,
    input_id: &str,
    sysname: &str,
    have_videotoolbox: bool,
) -> Vec<String> {
    let fps = cfg.capture.fps;
    let mut argv = vec![
        "ffmpeg".into(),
        "-hide_banner".into(),
        "-loglevel".into(),
        if cfg.encoder.mode == "mjpeg" {
            "error".into()
        } else {
            "info".into()
        },
    ];
    argv.extend(input_part(sysname, input_id, fps));
    argv.push("-an".into());
    argv.push("-vf".into());
    argv.push(scale_filter(cfg, fps));

    if cfg.encoder.mode == "mjpeg" {
        argv.extend([
            "-c:v".into(),
            "mjpeg".into(),
            "-q:v".into(),
            jpeg_q(cfg.capture.jpeg_quality).to_string(),
            "-f".into(),
            "mjpeg".into(),
            "pipe:1".into(),
        ]);
        return argv;
    }

    let br = cfg.encoder.bitrate_kbps;
    let gop = cfg.encoder.gop_frames;
    let buf = (br / 2).max(1);

    argv.extend([
        "-fflags".into(),
        "+nobuffer+flush_packets".into(),
        "-flags".into(),
        "+low_delay".into(),
        "-flush_packets".into(),
        "1".into(),
        "-muxdelay".into(),
        "0".into(),
        "-muxpreload".into(),
        "0".into(),
    ]);

    if cfg.encoder.mode == "hevc" && sysname == "Darwin" {
        argv.extend([
            "-c:v".into(),
            "hevc_videotoolbox".into(),
            "-allow_sw".into(),
            "1".into(),
            "-profile:v".into(),
            "main".into(),
            "-realtime".into(),
            "1".into(),
            "-bf".into(),
            "0".into(),
        ]);
    } else if sysname == "Darwin" && have_videotoolbox {
        argv.extend([
            "-c:v".into(),
            "h264_videotoolbox".into(),
            "-allow_sw".into(),
            "1".into(),
            "-profile:v".into(),
            "high".into(),
            "-realtime".into(),
            "1".into(),
            "-bf".into(),
            "0".into(),
        ]);
    } else {
        argv.extend([
            "-c:v".into(),
            "libx264".into(),
            "-preset".into(),
            "veryfast".into(),
            "-tune".into(),
            "zerolatency".into(),
            "-profile:v".into(),
            "high".into(),
            "-bf".into(),
            "0".into(),
            "-x264-params".into(),
            "nal-hrd=cbr:force-cfr=1:sliced-threads=1:rc-lookahead=0:sync-lookahead=0:bframes=0:repeat-headers=1".into(),
        ]);
    }

    argv.extend([
        "-b:v".into(),
        format!("{br}k"),
        "-maxrate".into(),
        format!("{br}k"),
        "-bufsize".into(),
        format!("{buf}k"),
        "-g".into(),
        gop.to_string(),
        "-keyint_min".into(),
        gop.to_string(),
        "-sc_threshold".into(),
        "0".into(),
        "-pix_fmt".into(),
        "yuv420p".into(),
        "-f".into(),
        "mp4".into(),
        "-movflags".into(),
        "frag_keyframe+empty_moov+default_base_moof".into(),
        "pipe:1".into(),
    ]);
    argv
}

pub fn sysname() -> &'static str {
    match std::env::consts::OS {
        "macos" => "Darwin",
        "windows" => "Windows",
        _ => "Linux",
    }
}

pub fn have_videotoolbox() -> bool {
    static CACHED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *CACHED.get_or_init(|| {
        std::process::Command::new("ffmpeg")
            .args(["-hide_banner", "-encoders"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains("h264_videotoolbox"))
            .unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;

    fn cfg() -> Config {
        Config::default()
    }

    fn flag_after<'a>(argv: &'a [String], flag: &str) -> Option<&'a str> {
        argv.iter()
            .position(|a| a == flag)
            .and_then(|i| argv.get(i + 1))
            .map(|s| s.as_str())
    }

    #[test]
    fn scale_never_upscales_uses_lanczos_and_fps() {
        let vf = scale_filter(&cfg(), 30);
        assert!(vf.contains("lanczos"), "{vf}");
        assert!(vf.contains("min(3840\\,iw"), "{vf}");
        assert!(vf.contains("min(4320\\,ih"), "{vf}");
        assert!(vf.contains("fps=30"), "{vf}");
        assert!(!vf.contains("select="), "{vf}");
    }

    #[test]
    fn scale_respects_custom_max_and_capture_scale() {
        let mut c = cfg();
        c.encoder.max_width = 1280;
        c.encoder.max_height = 720;
        c.capture.scale = 0.5;
        let vf = scale_filter(&c, 30);
        assert!(vf.contains("min(1280\\,iw*0.5)"), "{vf}");
        assert!(vf.contains("min(720\\,ih*0.5)"), "{vf}");
    }

    #[test]
    fn darwin_videotoolbox_low_latency_1080p30() {
        let c = cfg();
        let argv = build_ffmpeg_argv(&c, "3:", "Darwin", true);
        let joined = argv.join(" ");
        assert_eq!(argv[0], "ffmpeg");
        assert!(joined.contains("h264_videotoolbox"), "{joined}");
        assert_eq!(flag_after(&argv, "-realtime"), Some("1"));
        assert_eq!(flag_after(&argv, "-profile:v"), Some("high"));
        assert_eq!(flag_after(&argv, "-bf"), Some("0"));
        assert_eq!(flag_after(&argv, "-g"), Some("15"));
        assert_eq!(flag_after(&argv, "-keyint_min"), Some("15"));
        assert_eq!(flag_after(&argv, "-bufsize"), Some("10000k"));
        assert_eq!(flag_after(&argv, "-b:v"), Some("20000k"));
        assert!(joined.contains("flush_packets"), "{joined}");
        assert!(joined.contains("low_delay"), "{joined}");
        assert!(joined.contains("lanczos"), "{joined}");
        assert!(joined.contains("min(3840\\,iw"), "{joined}");
        assert!(!joined.contains("40000k"), "2s VBV must not appear: {joined}");
        assert!(!joined.contains("libx264"), "{joined}");
        let vf = flag_after(&argv, "-vf").unwrap();
        assert!(vf.contains("fps=30"), "{vf}");
        assert_eq!(flag_after(&argv, "-pix_fmt"), Some("yuv420p"));
    }

    #[test]
    fn libx264_veryfast_zerolatency_off_mac() {
        let mut c = cfg();
        c.encoder.bitrate_kbps = 8000;
        c.encoder.gop_frames = 30;
        let argv = build_ffmpeg_argv(&c, "desktop", "Linux", false);
        let joined = argv.join(" ");
        assert!(joined.contains("libx264"), "{joined}");
        assert_eq!(flag_after(&argv, "-preset"), Some("veryfast"));
        assert_eq!(flag_after(&argv, "-tune"), Some("zerolatency"));
        assert_eq!(flag_after(&argv, "-g"), Some("30"));
        assert_eq!(flag_after(&argv, "-bufsize"), Some("4000k"));
        assert!(joined.contains("nal-hrd=cbr"), "{joined}");
        assert_eq!(flag_after(&argv, "-bf"), Some("0"));
    }

    #[test]
    fn hevc_uses_same_vbv_and_gop() {
        let mut c = cfg();
        c.encoder.mode = "hevc".into();
        let argv = build_ffmpeg_argv(&c, "3:", "Darwin", true);
        assert!(argv.iter().any(|a| a == "hevc_videotoolbox"));
        assert_eq!(flag_after(&argv, "-bufsize"), Some("10000k"));
        assert_eq!(flag_after(&argv, "-g"), Some("15"));
        assert_eq!(flag_after(&argv, "-bf"), Some("0"));
        assert_eq!(flag_after(&argv, "-realtime"), Some("1"));
    }

    #[test]
    fn mjpeg_still_never_upscales() {
        let mut c = cfg();
        c.encoder.mode = "mjpeg".into();
        let argv = build_ffmpeg_argv(&c, "3:", "Darwin", true);
        let vf = flag_after(&argv, "-vf").unwrap();
        assert!(vf.contains("lanczos"), "{vf}");
        assert!(vf.contains("min(3840\\,iw"), "{vf}");
        assert!(argv.iter().any(|a| a == "mjpeg"));
    }
}
