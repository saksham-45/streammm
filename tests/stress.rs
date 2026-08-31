//! Performance and stress: 30 fps budget, 1080p-sized payloads, real ffmpeg encode.

use bytes::Bytes;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use streamaid::config::Config;
use streamaid::encoder::{build_ffmpeg_argv, have_videotoolbox, sysname};
use streamaid::frame::{make_jpeg, mp4_box, MjpegFramer, Mp4Framer, Unit};
use streamaid::hub::Hub;
use streamaid::protocol::{pack_media, unpack_media, TYPE_FRAG, TYPE_INIT};
use streamaid::ws::{decode_frame, encode_frame, OP_BIN};

const FRAME_PERIOD: Duration = Duration::from_micros(33_334); // 30 fps

fn gop_payload() -> Bytes {
    // ~10 Mbps / 2 fragments-per-second (GOP 15 @ 30 fps) ≈ 625 KB; use 64 KB
    // as a conservative per-fragment body for fan-out timing (encode is tested separately).
    Bytes::from(vec![0xAB; 64 * 1024])
}

#[test]
fn hub_publish_stays_under_frame_period_with_32_subscribers() {
    let hub = Hub::new();
    let mut subs = Vec::new();
    for _ in 0..32 {
        subs.push(hub.subscribe(8));
    }
    let payload = gop_payload();
    let n = 300; // 10 seconds at 30 fps
    let mut max = Duration::ZERO;
    let start = Instant::now();
    for _ in 0..n {
        let t = Instant::now();
        hub.publish_unit(TYPE_FRAG, payload.clone(), 1920, 1080);
        max = max.max(t.elapsed());
    }
    let mean = start.elapsed() / n;
    assert!(
        mean < Duration::from_micros(500),
        "mean publish {mean:?} (budget 500µs, frame is 33.3ms)"
    );
    assert!(
        max < FRAME_PERIOD,
        "worst publish {max:?} exceeded one 30 fps frame"
    );
    let got = subs[0].try_recv().is_some();
    assert!(got);
    drop(subs);
}

#[test]
fn hub_drop_oldest_under_backpressure() {
    let hub = Hub::new();
    let slow = hub.subscribe(2);
    let payload = gop_payload();
    for i in 0..100u32 {
        hub.publish_unit(TYPE_FRAG, payload.clone(), i, 0);
    }
    let a = slow.try_recv().unwrap();
    let b = slow.try_recv().unwrap();
    assert!(slow.try_recv().is_none());
    // last two publishes win
    assert_eq!(a.width, 98);
    assert_eq!(b.width, 99);
}

#[test]
fn ws_codec_300_gop_sized_frames_well_under_realtime() {
    let payload = pack_media(TYPE_FRAG, &vec![0xCD; 80 * 1024]);
    let n = 300;
    let start = Instant::now();
    for _ in 0..n {
        let wire = encode_frame(&payload, OP_BIN, false);
        let (op, data, consumed) = decode_frame(&wire);
        assert_eq!(op, Some(OP_BIN));
        assert_eq!(consumed, wire.len());
        let (kind, _) = unpack_media(&data).unwrap();
        assert_eq!(kind, TYPE_FRAG);
    }
    let elapsed = start.elapsed();
    let mean = elapsed / n;
    assert!(
        elapsed < Duration::from_millis(500),
        "300×80KiB frame codec took {elapsed:?}"
    );
    assert!(
        mean < Duration::from_millis(1),
        "mean codec {mean:?} should be << 33.3ms"
    );
}

#[test]
fn mjpeg_framer_splits_burst_of_30_jpegs() {
    let frame = make_jpeg(1920, 1080, &vec![0x11; 4096]);
    let mut burst = Vec::new();
    for _ in 0..30 {
        burst.extend_from_slice(&frame);
    }
    let mut fr = MjpegFramer::new();
    let t = Instant::now();
    let out = fr.push(&burst);
    let dt = t.elapsed();
    assert_eq!(out.len(), 30);
    assert_eq!(out[0], frame);
    assert!(
        dt < FRAME_PERIOD,
        "framing 30 JPEGs took {dt:?} (must be < one frame period)"
    );
}

#[test]
fn mp4_framer_handles_init_plus_60_fragments_in_one_push() {
    let mut init = mp4_box(b"ftyp", b"isom");
    init.extend_from_slice(&mp4_box(b"moov", &[0u8; 256]));
    let mut frag = mp4_box(b"moof", &[1u8; 64]);
    frag.extend_from_slice(&mp4_box(b"mdat", &vec![2u8; 32 * 1024]));
    let mut all = init.clone();
    for _ in 0..60 {
        all.extend_from_slice(&frag);
    }
    let mut p = Mp4Framer::new();
    let t = Instant::now();
    let units = p.push(&all);
    let dt = t.elapsed();
    assert_eq!(units.len(), 61);
    match &units[0] {
        Unit::Init(b) => assert_eq!(b, &init),
        other => panic!("{other:?}"),
    }
    assert!(matches!(units[1], Unit::Fragment(_)));
    assert!(
        dt < FRAME_PERIOD,
        "framing 60 fragments took {dt:?}"
    );
}

/// Rebuild a lavfi command that keeps the shipped encoder/mux flags (not a second encoder).
fn lavfi_from_shipped(cfg: &Config, seconds: &str) -> Vec<String> {
    let shipped = build_ffmpeg_argv(cfg, "unused", sysname(), have_videotoolbox());
    let an = shipped
        .iter()
        .position(|a| a == "-an")
        .expect("shipped argv has -an");
    let mut out = vec![
        "ffmpeg".into(),
        "-hide_banner".into(),
        "-nostdin".into(),
        "-loglevel".into(),
        "error".into(),
        "-f".into(),
        "lavfi".into(),
        "-i".into(),
        format!("testsrc2=size=1920x1080:rate={}", cfg.capture.fps),
        "-t".into(),
        seconds.into(),
    ];
    out.extend(shipped[an..].iter().cloned());
    out
}

#[test]
fn ffmpeg_1080p30_encode_is_realtime_and_frames_as_fmp4() {
    let cfg = Config::default();
    assert_eq!(cfg.capture.fps, 30);
    assert_eq!(cfg.encoder.gop_frames, 15);
    let argv = lavfi_from_shipped(&cfg, "2");
    // shipped flags must still be in the encode command
    let joined = argv.join(" ");
    assert!(joined.contains("lanczos"));
    assert!(joined.contains("-bf"));
    assert!(joined.contains("10000k"), "0.5s VBV of 20 Mbps: {joined}");
    assert!(joined.contains("20000k"), "{joined}");
    assert!(joined.contains("low_delay"));

    let t = Instant::now();
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn ffmpeg");
    let wall = t.elapsed();
    assert!(
        out.status.success(),
        "ffmpeg failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // 2s of 30 fps plus startup; hardware encode should not take 4× realtime
    assert!(
        wall < Duration::from_secs(8),
        "1080p30 × 2s encode took {wall:?}"
    );

    let mut fr = Mp4Framer::new();
    let units = fr.push(&out.stdout);
    let inits = units
        .iter()
        .filter(|u| matches!(u, Unit::Init(_)))
        .count();
    let frags = units
        .iter()
        .filter(|u| matches!(u, Unit::Fragment(_)))
        .count();
    assert_eq!(inits, 1, "expected one fMP4 init, units={}", units.len());
    // GOP 15 @ 30 fps × 2s ≈ 4 keyframes; allow encoder preroll
    assert!(
        frags >= 3,
        "expected several keyframe fragments, got {frags} (stdout {} bytes)",
        out.stdout.len()
    );
}

#[tokio::test]
async fn sixteen_websocket_clients_keep_up_at_30fps_burst() {
    use futures_util::StreamExt;
    use streamaid::config::Config;
    use streamaid::server::App;
    use tokio::net::TcpListener;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = Config::default();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    let hub = app.hub.clone();

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    hub.publish_init(Bytes::from_static(b"init-bytes"), 1920, 1080);

    let mut clients = Vec::new();
    for _ in 0..16 {
        let url = format!("ws://{addr}/stream.ws");
        let (ws, resp) = connect_async(&url).await.expect("ws");
        assert_eq!(resp.status().as_u16(), 101);
        clients.push(ws);
    }

    let n_pub = 60u32; // 2 seconds at 30 fps
    let payload = gop_payload();
    let t = Instant::now();
    for i in 0..n_pub {
        hub.publish_unit(TYPE_FRAG, payload.clone(), i, 0);
    }
    let publish_dt = t.elapsed();
    assert!(
        publish_dt < FRAME_PERIOD * 4,
        "publishing 60 frames to 16 WS clients via hub took {publish_dt:?}"
    );

    // Each client should have received init plus some fragments (drop-oldest cap is 8).
    for ws in clients.iter_mut() {
        let mut saw_init = false;
        let mut n = 0;
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline && n < 4 {
            match tokio::time::timeout(Duration::from_millis(400), ws.next()).await {
                Ok(Some(Ok(Message::Binary(bin)))) => {
                    let (k, _) = unpack_media(&bin).unwrap();
                    if k == TYPE_INIT {
                        saw_init = true;
                    }
                    n += 1;
                }
                _ => break,
            }
        }
        let _ = ws.close(None).await;
        assert!(saw_init, "client missed init envelope");
        assert!(n >= 2, "client only got {n} messages");
    }
}
