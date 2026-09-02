//! HTTP API on the real Axum router (no capture required).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use streamaid::config::Config;
use streamaid::frame::mp4_box;
use streamaid::protocol::TYPE_FRAG;
use streamaid::server::App;
use tokio::net::TcpListener;
use tower::ServiceExt;

fn temp_app() -> (tempfile::TempDir, std::sync::Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = Config::default();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    (dir, app)
}

async fn body_json(res: axum::response::Response) -> Value {
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).unwrap()
}

#[tokio::test]
async fn wol_rejects_bad_mac() {
    let (_dir, app) = temp_app();
    let router = app.router();
    let res = router
        .oneshot(
            Request::post("/api/wol")
                .header("content-type", "application/json")
                .body(Body::from(json!({"mac": "nope"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn permissions_open_rejects_unknown_pane() {
    let (_dir, app) = temp_app();
    let router = app.router();
    let res = router
        .oneshot(
            Request::post("/api/permissions/open")
                .header("content-type", "application/json")
                .body(Body::from(json!({"which": "nope"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn config_post_clamps_and_round_trips() {
    let (_dir, app) = temp_app();
    let router = app.router();
    let res = router
        .clone()
        .oneshot(
            Request::post("/api/config")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"encoder": {"bitrate_kbps": 50, "gop_frames": 12}}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let v = body_json(res).await;
    assert_eq!(v["applied"], true);

    let res = router
        .oneshot(Request::get("/api/config").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let cfg = body_json(res).await;
    assert_eq!(cfg["encoder"]["bitrate_kbps"], 2000); // clamped from 50
    assert_eq!(cfg["encoder"]["gop_frames"], 12);
    assert_eq!(cfg["encoder"]["mode"], "ffmpeg");
}

#[tokio::test]
async fn token_rejects_unauthorized_and_accepts_bearer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    let router = app.router();

    let res = router
        .clone()
        .oneshot(Request::get("/api/config").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

    let res = router
        .clone()
        .oneshot(
            Request::get("/api/config")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = router
        .clone()
        .oneshot(
            Request::get("/api/config?token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = router
        .clone()
        .oneshot(
            Request::get("/api/config")
                .header("Cookie", "streamaid_token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);

    let res = router
        .oneshot(
            Request::get("/api/status")
                .header("Cookie", "streamaid_token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let st = body_json(res).await;
    assert!(st.get("capture").is_some());
}

#[tokio::test]
async fn public_ui_does_not_need_token() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    let router = app.router();
    let res = router
        .oneshot(Request::get("/").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn mp4_stream_is_chunked_no_transform_and_starts_with_init() {
    let (_dir, app) = temp_app();
    let mut init = mp4_box(b"ftyp", b"isom");
    init.extend_from_slice(&mp4_box(b"moov", b"trak"));
    let mut frag = mp4_box(b"moof", b"mfhd");
    frag.extend_from_slice(&mp4_box(b"mdat", b"mdat-bytes"));
    app.hub.publish_init(Bytes::from(init.clone()), 1920, 1080);
    app.hub
        .publish_unit(TYPE_FRAG, Bytes::from(frag.clone()), 0, 0);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });

    let url = format!("http://{addr}/stream.mp4");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap();
    let res = client.get(&url).send().await.expect("GET /stream.mp4");
    assert_eq!(res.status(), 200);
    let headers = res.headers();
    let cc = headers
        .get("cache-control")
        .unwrap()
        .to_str()
        .unwrap()
        .to_lowercase();
    assert!(cc.contains("no-store"), "{cc}");
    assert!(cc.contains("no-transform"), "{cc}");
    let te = headers
        .get("transfer-encoding")
        .map(|v| v.to_str().unwrap().to_lowercase())
        .unwrap_or_default();
    assert!(
        te.contains("chunked") || res.content_length().is_none(),
        "expected chunked streaming, te={te}"
    );
    use futures_util::StreamExt;
    let mut stream = res.bytes_stream();
    let mut buf = Vec::new();
    let deadline = tokio::time::sleep(std::time::Duration::from_secs(2));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            chunk = stream.next() => {
                match chunk {
                    Some(Ok(b)) => {
                        buf.extend_from_slice(&b);
                        if buf.len() >= init.len() {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            _ = &mut deadline => break,
        }
    }
    assert!(buf.len() >= init.len(), "got {} bytes", buf.len());
    assert_eq!(&buf[..init.len()], init.as_slice());
}

#[tokio::test]
async fn concurrent_status_does_not_error() {
    let (_dir, app) = temp_app();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app.router();
    tokio::spawn(async move {
        axum::serve(listener, router).await.ok();
    });
    let url = format!("http://{addr}/api/status");
    let client = reqwest::Client::new();
    let mut joins = Vec::new();
    for _ in 0..16 {
        let client = client.clone();
        let url = url.clone();
        joins.push(tokio::spawn(async move {
            let mut ok = 0;
            for _ in 0..25 {
                let res = client.get(&url).send().await.unwrap();
                if res.status().is_success() {
                    let v: Value = res.json().await.unwrap();
                    if v.get("capture").is_some() && v.get("stream").is_some() {
                        ok += 1;
                    }
                }
            }
            ok
        }));
    }
    let mut total = 0;
    for j in joins {
        total += j.await.unwrap();
    }
    assert_eq!(total, 16 * 25);
}

fn token_app() -> (tempfile::TempDir, std::sync::Arc<App>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    (dir, app)
}

fn flip_pin(pin: &str) -> String {
    let mut c: Vec<char> = pin.chars().collect();
    let last = c.len() - 1;
    c[last] = if c[last] == '0' { '1' } else { '0' };
    c.into_iter().collect()
}

#[tokio::test]
async fn pin_mint_redeem_session_gates_watch_not_stream_token() {
    let (_dir, app) = token_app();
    let router = app.router();

    let denied = router
        .clone()
        .oneshot(Request::get("/stream.mp4").body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(denied.status(), StatusCode::UNAUTHORIZED);

    let host = router
        .clone()
        .oneshot(
            Request::get("/stream.mp4?token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(host.status(), StatusCode::OK);

    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(minted.status(), StatusCode::OK);
    let pin_body = body_json(minted).await;
    let pin = pin_body["pin"].as_str().unwrap().to_string();
    assert_eq!(pin.len(), 6);
    assert!(pin.chars().all(|c| c.is_ascii_digit()));

    let shown = router
        .clone()
        .oneshot(
            Request::get("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let shown = body_json(shown).await;
    assert_eq!(shown["pin"], pin);

    let bad = router
        .clone()
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": flip_pin(&pin)}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad.status(), StatusCode::UNAUTHORIZED);

    let ok = router
        .clone()
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": pin}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(ok.status(), StatusCode::OK);
    let cookies: Vec<String> = ok
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    let set_cookie = cookies.join("; ");
    assert!(set_cookie.contains("streamaid_session="));
    assert!(
        set_cookie.contains("Max-Age=86400"),
        "session cookie must last a day, got {set_cookie}"
    );
    let sess = body_json(ok).await;
    let session = sess["session"].as_str().unwrap().to_string();
    assert!(!session.is_empty());
    assert_eq!(
        sess["expires_in_s"].as_u64().unwrap_or(0),
        86400,
        "redeemed session must last a day"
    );

    let watch = router
        .oneshot(
            Request::get("/stream.mp4")
                .header("Cookie", format!("streamaid_session={session}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(watch.status(), StatusCode::OK);
}

#[tokio::test]
async fn pin_regenerate_invalidates_previous() {
    let (_dir, app) = token_app();
    let router = app.router();
    let a = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin_a = body_json(a).await["pin"].as_str().unwrap().to_string();
    let b = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin_b = body_json(b).await["pin"].as_str().unwrap().to_string();
    assert_ne!(pin_a, pin_b);
    let old = router
        .clone()
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": pin_a}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    let new = router
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": pin_b}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(new.status(), StatusCode::OK);
}

#[tokio::test]
async fn pin_rate_limit_and_expire() {
    use std::sync::Arc;
    use std::time::Duration;
    use streamaid::computer_use::DoneModel;
    use streamaid::input::FakeInjector;
    use streamaid::otp::{FakeClock, FAIL_LIMIT, LOCKOUT, PIN_TTL};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let clock = FakeClock::new();
    let app = App::new_for_test(
        cfg,
        path,
        clock.clone(),
        FakeInjector::new(),
        Arc::new(DoneModel),
    );
    let router = app.router();
    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    let wrong = flip_pin(&pin);
    for _ in 0..FAIL_LIMIT {
        let res = router
            .clone()
            .oneshot(
                Request::post("/api/otp/redeem")
                    .header("content-type", "application/json")
                    .body(Body::from(json!({"pin": wrong}).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
    let limited = router
        .clone()
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": wrong}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(limited.status(), StatusCode::TOO_MANY_REQUESTS);

    let dir2 = tempfile::tempdir().unwrap();
    let path2 = dir2.path().join("config.json");
    let mut cfg2 = Config::default();
    cfg2.token = "s3cret".into();
    streamaid::config::save(&cfg2, &path2).unwrap();
    let clock2 = FakeClock::new();
    let app2 = App::new_for_test(
        cfg2,
        path2,
        clock2.clone(),
        FakeInjector::new(),
        Arc::new(DoneModel),
    );
    let router2 = app2.router();
    let minted2 = router2
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin2 = body_json(minted2).await["pin"]
        .as_str()
        .unwrap()
        .to_string();
    clock2.advance(PIN_TTL + Duration::from_secs(1));
    let expired = router2
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": pin2}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);
    let _ = LOCKOUT;
}

#[tokio::test]
async fn computer_use_forbidden_when_disabled_and_stub_when_enabled() {
    use std::sync::Arc;
    use streamaid::computer_use::StubClickTypeModel;
    use streamaid::input::{FakeInjector, Injected};
    use streamaid::otp::FakeClock;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let fake = FakeInjector::new();
    let app = App::new_for_test(
        cfg,
        path,
        FakeClock::new(),
        fake.clone(),
        Arc::new(StubClickTypeModel::default()),
    );
    let router = app.clone().router();
    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    let redeemed = router
        .clone()
        .oneshot(
            Request::post("/api/otp/redeem")
                .header("content-type", "application/json")
                .body(Body::from(json!({"pin": pin}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let session = body_json(redeemed).await["session"]
        .as_str()
        .unwrap()
        .to_string();

    let off = router
        .clone()
        .oneshot(
            Request::post("/api/computer-use")
                .header("Cookie", format!("streamaid_session={session}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"task": "type hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(off.status(), StatusCode::FORBIDDEN);
    let err = body_json(off).await;
    assert!(err["error"].as_str().unwrap().contains("disabled"));

    app.cfg.lock().control.ai_enabled = true;
    let on = router
        .oneshot(
            Request::post("/api/computer-use")
                .header("Cookie", format!("streamaid_session={session}"))
                .header("content-type", "application/json")
                .body(Body::from(json!({"task": "type hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(on.status(), StatusCode::OK);
    let body = body_json(on).await;
    assert_eq!(body["ok"], true);
    assert_eq!(
        fake.recorded(),
        vec![
            Injected::click(0.5, 0.5),
            Injected::Type {
                text: "hello".into()
            }
        ]
    );
}

#[tokio::test]
async fn computer_use_grabs_snap_jpeg_not_fragment() {
    use bytes::Bytes;
    use std::sync::{Arc, Mutex};
    use streamaid::computer_use::ActionModel;
    use streamaid::input::{Action, FakeInjector};
    use streamaid::otp::FakeClock;
    use streamaid::protocol::{TYPE_FRAG, TYPE_SNAP};

    struct RecordJpeg {
        got: Mutex<Vec<Vec<u8>>>,
    }
    impl ActionModel for RecordJpeg {
        fn plan(&self, _task: &str, _step: u32, jpeg: &[u8]) -> Vec<Action> {
            self.got.lock().unwrap().push(jpeg.to_vec());
            vec![Action::Done]
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    cfg.control.ai_enabled = true;
    streamaid::config::save(&cfg, &path).unwrap();
    let rec = Arc::new(RecordJpeg {
        got: Mutex::new(Vec::new()),
    });
    let app = App::new_for_test(
        cfg,
        path,
        FakeClock::new(),
        FakeInjector::new(),
        rec.clone(),
    );
    app.hub.publish_unit(
        TYPE_FRAG,
        Bytes::from_static(b"moof-not-a-jpeg"),
        1920,
        1080,
    );
    app.hub
        .publish_unit(TYPE_SNAP, Bytes::from_static(b"\xff\xd8SNAP"), 0, 0);
    let applied = app.run_computer_use("do it").await;
    assert_eq!(applied, vec![Action::Done]);
    let frames = rec.got.lock().unwrap().clone();
    assert!(!frames.is_empty());
    assert_eq!(frames[0], b"\xff\xd8SNAP");
}

#[tokio::test]
async fn host_cancel_stops_running_ai_loop() {
    use std::sync::Arc;
    use streamaid::computer_use::ActionModel;
    use streamaid::input::{Action, FakeInjector, Injected};
    use streamaid::otp::FakeClock;

    struct WaitThenClick;
    impl ActionModel for WaitThenClick {
        fn plan(&self, _task: &str, step: u32, _jpeg: &[u8]) -> Vec<Action> {
            match step {
                0 => vec![Action::Wait { ms: 4000 }],
                _ => vec![Action::click(0.1, 0.1)],
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    cfg.control.ai_enabled = true;
    streamaid::config::save(&cfg, &path).unwrap();
    let fake = FakeInjector::new();
    let app = App::new_for_test(
        cfg,
        path,
        FakeClock::new(),
        fake.clone(),
        Arc::new(WaitThenClick),
    );
    let router = app.clone().router();
    let run = {
        let app = app.clone();
        tokio::spawn(async move { app.run_computer_use("slow").await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    let cancel = router
        .oneshot(
            Request::post("/api/computer-use/cancel")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(cancel.status(), StatusCode::OK);
    let applied = tokio::time::timeout(std::time::Duration::from_secs(2), run)
        .await
        .expect("join")
        .unwrap();
    assert!(
        !applied.iter().any(|a| matches!(a, Action::Click { .. })),
        "cancel must stop before click, got {applied:?}"
    );
    assert!(!fake
        .recorded()
        .iter()
        .any(|e| matches!(e, Injected::Click { .. })));
}

#[tokio::test]
async fn computer_use_file_panel_ops_hit_real_handlers() {
    use std::sync::Arc;
    use streamaid::computer_use::ActionModel;
    use streamaid::input::{Action, FakeInjector};
    use streamaid::otp::FakeClock;

    struct MkdirThenDone;
    impl ActionModel for MkdirThenDone {
        fn plan(&self, _task: &str, step: u32, _jpeg: &[u8]) -> Vec<Action> {
            match step {
                0 => vec![Action::FileManage {
                    op: "mkdir".into(),
                    name: "FromHttp".into(),
                    names: vec!["FromHttp".into()],
                    root: "inbox".into(),
                    path: String::new(),
                    to: String::new(),
                    to_root: String::new(),
                    to_path: String::new(),
                }],
                1 => vec![
                    Action::FileManage {
                        op: "rename".into(),
                        name: "FromHttp".into(),
                        names: vec!["FromHttp".into()],
                        root: "inbox".into(),
                        path: String::new(),
                        to: "RenamedHttp".into(),
                        to_root: String::new(),
                        to_path: String::new(),
                    },
                    Action::Done,
                ],
                _ => vec![Action::Done],
            }
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    cfg.control.ai_enabled = true;
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new_for_test(
        cfg,
        path,
        FakeClock::new(),
        FakeInjector::new(),
        Arc::new(MkdirThenDone),
    );
    let router = app.clone().router();
    let res = router
        .oneshot(
            Request::post("/api/computer-use")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(json!({"task": "make a folder"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
    let body = body_json(res).await;
    assert_eq!(body["ok"], true);
    assert!(app
        .files
        .join_under("inbox", "", "RenamedHttp")
        .unwrap()
        .is_dir());
    assert!(!app
        .files
        .join_under("inbox", "", "FromHttp")
        .unwrap()
        .exists());
}

#[tokio::test]
async fn production_app_wires_llm_model() {
    let (_dir, app) = temp_app();
    assert_eq!(app.model.lock().kind(), "llm");
}

#[tokio::test]
async fn percent_encoded_token_cookie_authorizes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "p@ss/w d+".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new(cfg, path);
    let router = app.router();
    let res = router
        .oneshot(
            Request::get("/api/config")
                .header("Cookie", "streamaid_token=p%40ss%2Fw%20d%2B")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
}

#[tokio::test]
async fn host_login_sets_token_cookie_and_rejects_bad_input() {
    let (_dir, app) = token_app();
    let router = app.router();

    let malformed = post_json(router.clone(), "/api/login", Body::from("{")).await;
    let (st, _) = json_error_body(malformed).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    for body in [
        json!({}),
        json!({"token": ""}),
        json!({"token": "   "}),
        json!({"token": 1}),
    ] {
        let res = post_json(router.clone(), "/api/login", Body::from(body.to_string())).await;
        let (st, err) = json_error_body(res).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "body={body} err={err}");
    }

    let wrong = post_json(
        router.clone(),
        "/api/login",
        Body::from(json!({"token": "nope"}).to_string()),
    )
    .await;
    let (st, _) = json_error_body(wrong).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let ok = post_json(
        router.clone(),
        "/api/login",
        Body::from(json!({"token": "s3cret"}).to_string()),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let cookies: Vec<String> = ok
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies.join("; ").contains("streamaid_token="),
        "login must set host token cookie, got {cookies:?}"
    );
    let body = body_json(ok).await;
    assert_eq!(body["ok"], true);

    let authed = router
        .oneshot(
            Request::get("/api/config")
                .header("Cookie", "streamaid_token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(authed.status(), StatusCode::OK);
}

async fn json_error_body(res: axum::response::Response) -> (StatusCode, Value) {
    let status = res.status();
    assert!(
        !status.is_server_error(),
        "fail-closed API must not 5xx, got {status}"
    );
    let v = body_json(res).await;
    assert!(
        v.get("error")
            .and_then(|e| e.as_str())
            .filter(|s| !s.is_empty())
            .is_some(),
        "expected JSON error field, got {v}"
    );
    (status, v)
}

async fn post_json(router: axum::Router, path: &str, body: Body) -> axum::response::Response {
    router
        .oneshot(
            Request::post(path)
                .header("content-type", "application/json")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn otp_redeem_rejects_malformed_empty_and_non_string_pin() {
    let (_dir, app) = token_app();
    let router = app.router();
    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    assert_eq!(pin.len(), 6);

    let malformed = post_json(router.clone(), "/api/otp/redeem", Body::from("{")).await;
    let (st, _) = json_error_body(malformed).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    for body in [
        json!({}),
        json!({"pin": ""}),
        json!({"pin": "   "}),
        json!({"pin": 123456}),
        json!({"pin": true}),
        json!({"pin": null}),
    ] {
        let res = post_json(
            router.clone(),
            "/api/otp/redeem",
            Body::from(body.to_string()),
        )
        .await;
        let (st, err) = json_error_body(res).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "body={body} err={err}");
    }

    let ok = post_json(
        router,
        "/api/otp/redeem",
        Body::from(json!({"pin": pin}).to_string()),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let cookies: Vec<String> = ok
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies.join("; ").contains("streamaid_session="),
        "valid redeem must set session cookie, got {cookies:?}"
    );
    let sess = body_json(ok).await;
    assert!(
        sess["session"].as_str().filter(|s| !s.is_empty()).is_some(),
        "valid redeem must return session, got {sess}"
    );
}

#[tokio::test]
async fn unattended_password_redeems_after_pin_expires() {
    use std::sync::Arc;
    use std::time::Duration;
    use streamaid::computer_use::DoneModel;
    use streamaid::input::FakeInjector;
    use streamaid::otp::{FakeClock, PIN_TTL};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let cfg = Config::default();
    streamaid::config::save(&cfg, &path).unwrap();
    let clock = FakeClock::new();
    let app = App::new_for_test(
        cfg,
        path,
        clock.clone(),
        FakeInjector::new(),
        Arc::new(DoneModel),
    );
    let router = app.router();

    let short = post_json(
        router.clone(),
        "/api/config",
        Body::from(json!({"access": {"unattended": true, "password": "short"}}).to_string()),
    )
    .await;
    assert_eq!(short.status(), StatusCode::BAD_REQUEST);

    let set = post_json(
        router.clone(),
        "/api/config",
        Body::from(json!({"access": {"unattended": true, "password": "s3cret!!"}}).to_string()),
    )
    .await;
    assert_eq!(set.status(), StatusCode::OK);

    let shown = router
        .clone()
        .oneshot(Request::get("/api/config").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let shown = body_json(shown).await;
    assert_eq!(shown["access"]["unattended"], true);
    assert_eq!(shown["access"]["password_set"], true);
    assert!(shown["access"].get("password_hash").is_none());

    let minted = router
        .clone()
        .oneshot(Request::post("/api/otp").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    clock.advance(PIN_TTL + Duration::from_secs(1));

    let expired = post_json(
        router.clone(),
        "/api/otp/redeem",
        Body::from(json!({"pin": pin}).to_string()),
    )
    .await;
    assert_eq!(expired.status(), StatusCode::UNAUTHORIZED);

    let ok = post_json(
        router,
        "/api/otp/redeem",
        Body::from(json!({"pin": "s3cret!!"}).to_string()),
    )
    .await;
    assert_eq!(ok.status(), StatusCode::OK);
    let sess = body_json(ok).await;
    assert!(
        sess["session"].as_str().filter(|s| !s.is_empty()).is_some(),
        "unattended password must issue a session after the PIN expires, got {sess}"
    );
}

#[tokio::test]
async fn config_post_rejects_malformed_non_object_and_unauthorized() {
    let (_dir, app) = token_app();
    let router = app.router();

    let unauth = post_json(
        router.clone(),
        "/api/config",
        Body::from(json!({"encoder": {"gop_frames": 12}}).to_string()),
    )
    .await;
    let (st, _) = json_error_body(unauth).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let malformed = router
        .clone()
        .oneshot(
            Request::post("/api/config")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(malformed).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let non_object = router
        .oneshot(
            Request::post("/api/config")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(json!([1, 2, 3]).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(non_object).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn computer_use_rejects_unauthorized_malformed_missing_task_and_ai_off() {
    use std::sync::Arc;
    use streamaid::computer_use::DoneModel;
    use streamaid::input::FakeInjector;
    use streamaid::otp::FakeClock;

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut cfg = Config::default();
    cfg.token = "s3cret".into();
    streamaid::config::save(&cfg, &path).unwrap();
    let app = App::new_for_test(
        cfg,
        path,
        FakeClock::new(),
        FakeInjector::new(),
        Arc::new(DoneModel),
    );
    let router = app.clone().router();

    let no_auth = post_json(
        router.clone(),
        "/api/computer-use",
        Body::from(json!({"task": "type hello"}).to_string()),
    )
    .await;
    let (st, _) = json_error_body(no_auth).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    let redeemed = post_json(
        router.clone(),
        "/api/otp/redeem",
        Body::from(json!({"pin": pin}).to_string()),
    )
    .await;
    let session = body_json(redeemed).await["session"]
        .as_str()
        .unwrap()
        .to_string();
    let cookie = format!("streamaid_session={session}");

    let ai_off = router
        .clone()
        .oneshot(
            Request::post("/api/computer-use")
                .header("Cookie", cookie.clone())
                .header("content-type", "application/json")
                .body(Body::from(json!({"task": "type hello"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, err) = json_error_body(ai_off).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    assert!(err["error"].as_str().unwrap().contains("disabled"));

    app.cfg.lock().control.ai_enabled = true;

    let malformed = router
        .clone()
        .oneshot(
            Request::post("/api/computer-use")
                .header("Cookie", cookie.clone())
                .header("content-type", "application/json")
                .body(Body::from("{"))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(malformed).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    for body in [
        json!({}),
        json!({"task": ""}),
        json!({"task": "   "}),
        json!({"task": 1}),
    ] {
        let res = router
            .clone()
            .oneshot(
                Request::post("/api/computer-use")
                    .header("Cookie", cookie.clone())
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let (st, err) = json_error_body(res).await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "body={body} err={err}");
    }
}

#[tokio::test]
async fn files_put_list_download_and_reject_traversal() {
    use streamaid::files::encode_b64;

    let (_dir, app) = token_app();
    let router = app.clone().router();
    let unauth = post_json(
        router.clone(),
        "/api/files",
        Body::from(json!({"name": "a.txt", "data": encode_b64(b"hi")}).to_string()),
    )
    .await;
    let (st, _) = json_error_body(unauth).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    let put = router
        .clone()
        .oneshot(
            Request::post("/api/files")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "hello.txt", "data": encode_b64(b"hello-inbox")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let body = body_json(put).await;
    assert_eq!(body["ok"], true);
    assert_eq!(body["name"], "hello.txt");

    let traversal = router
        .clone()
        .oneshot(
            Request::post("/api/files")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "../evil.txt", "data": encode_b64(b"no")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(traversal).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    let list = router
        .clone()
        .oneshot(
            Request::get("/api/files")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list.status(), StatusCode::OK);
    let listed = body_json(list).await;
    let files = listed["files"].as_array().unwrap();
    assert!(files.iter().any(|f| f["name"] == "hello.txt"));

    let dl = router
        .clone()
        .oneshot(
            Request::get("/api/files/download?name=hello.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(dl.status(), StatusCode::OK);
    let bytes = dl.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&bytes[..], b"hello-inbox");

    let big = vec![9u8; streamaid::files::MAX_CHUNK + 32];
    app.files.put_bytes("big.bin", &big).unwrap();
    let big_dl = router
        .clone()
        .oneshot(
            Request::get("/api/files/download?name=big.bin")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(big_dl.status(), StatusCode::OK);
    let cl = big.len().to_string();
    assert_eq!(
        big_dl.headers().get("content-length").and_then(|v| v.to_str().ok()),
        Some(cl.as_str())
    );
    let got = big_dl.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&got[..], &big[..]);

    let mkdir = router
        .clone()
        .oneshot(
            Request::post("/api/files/mkdir")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "SubDir"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mkdir.status(), StatusCode::OK);
    let made = body_json(mkdir).await;
    assert_eq!(made["mkdir"], true);
    assert_eq!(made["name"], "SubDir");
    assert_eq!(made["dir"], true);

    let mkdir_again = router
        .clone()
        .oneshot(
            Request::post("/api/files/mkdir")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(json!({"name": "SubDir"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(mkdir_again.status(), StatusCode::CONFLICT);

    let del_dir = router
        .clone()
        .oneshot(
            Request::delete("/api/files?name=SubDir")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_dir.status(), StatusCode::OK);
    let del_dir_body = body_json(del_dir).await;
    assert_eq!(del_dir_body["deleted"], true);
    assert_eq!(del_dir_body["dir"], true);

    app.files.mkdir_at("inbox", "", "Nested").unwrap();
    app.files
        .put_bytes_at("inbox", "Nested", "x.txt", b"z")
        .unwrap();
    let del_tree = router
        .clone()
        .oneshot(
            Request::delete("/api/files?name=Nested")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del_tree.status(), StatusCode::OK);
    assert!(!app.files.join_under("inbox", "", "Nested").unwrap().exists());

    app.files.put_bytes("rename-me.txt", b"hi").unwrap();
    let renamed = router
        .clone()
        .oneshot(
            Request::post("/api/files/rename")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "rename-me.txt", "to": "renamed.txt"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(renamed.status(), StatusCode::OK);
    let renamed_body = body_json(renamed).await;
    assert_eq!(renamed_body["renamed"], true);
    assert_eq!(renamed_body["name"], "renamed.txt");
    let clash = router
        .clone()
        .oneshot(
            Request::post("/api/files/rename")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "renamed.txt", "to": "big.bin"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(clash.status(), StatusCode::CONFLICT);

    app.files.mkdir_at("inbox", "", "Pack").unwrap();
    app.files
        .put_bytes_at("inbox", "Pack", "note.txt", b"hello-zip")
        .unwrap();
    let zip_dl = router
        .clone()
        .oneshot(
            Request::get("/api/files/download?name=Pack")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(zip_dl.status(), StatusCode::OK);
    assert_eq!(
        zip_dl.headers().get("content-type").and_then(|v| v.to_str().ok()),
        Some("application/zip")
    );
    let disp = zip_dl
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(disp.contains("Pack.zip"), "{disp}");
    let zip_bytes = zip_dl.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&zip_bytes[..4], b"PK\x03\x04");
    assert!(
        zip_bytes.windows(b"hello-zip".len()).any(|w| w == b"hello-zip"),
        "zip must contain the folder file"
    );

    app.files.put_bytes("sel-a.txt", b"AAA").unwrap();
    app.files.put_bytes("sel-b.txt", b"BBB").unwrap();
    let multi_dl = router
        .clone()
        .oneshot(
            Request::get("/api/files/download?name=sel-a.txt&name=sel-b.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(multi_dl.status(), StatusCode::OK);
    assert_eq!(
        multi_dl
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("application/zip")
    );
    let multi_disp = multi_dl
        .headers()
        .get("content-disposition")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(multi_disp.contains("files.zip"), "{multi_disp}");
    let multi_bytes = multi_dl.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&multi_bytes[..4], b"PK\x03\x04");
    assert!(multi_bytes.windows(b"AAA".len()).any(|w| w == b"AAA"));
    assert!(multi_bytes.windows(b"BBB".len()).any(|w| w == b"BBB"));

    let bulk_copy = router
        .clone()
        .oneshot(
            Request::post("/api/files/copy")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"names": ["sel-a.txt", "sel-b.txt"], "root": "inbox", "toRoot": "inbox", "toPath": "Pack"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bulk_copy.status(), StatusCode::OK);
    assert!(app
        .files
        .join_under("inbox", "Pack", "sel-a.txt")
        .unwrap()
        .is_file());

    let bulk_del = router
        .clone()
        .oneshot(
            Request::delete("/api/files?name=sel-a.txt&name=sel-b.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bulk_del.status(), StatusCode::OK);
    assert!(app.files.get_bytes("sel-a.txt").is_err());

    let copied = router
        .clone()
        .oneshot(
            Request::post("/api/files/copy")
                .header("Authorization", "Bearer s3cret")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "hello.txt", "root": "inbox", "toRoot": "inbox"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(copied.status(), StatusCode::OK);
    let copied_body = body_json(copied).await;
    assert_eq!(copied_body["copied"], true);
    assert_eq!(copied_body["name"], "hello-1.txt");

    let del = router
        .clone()
        .oneshot(
            Request::delete("/api/files?name=hello.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(del.status(), StatusCode::OK);
    let deleted = body_json(del).await;
    assert_eq!(deleted["deleted"], true);
    assert_eq!(deleted["name"], "hello.txt");

    let gone = router
        .clone()
        .oneshot(
            Request::delete("/api/files?name=hello.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);

    let traversal_del = router
        .oneshot(
            Request::delete("/api/files?name=../evil.txt")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(traversal_del).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn viewer_files_forbidden_until_control_enabled() {
    use streamaid::files::encode_b64;

    let (_dir, app) = token_app();
    let router = app.clone().router();
    let minted = router
        .clone()
        .oneshot(
            Request::post("/api/otp")
                .header("Authorization", "Bearer s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let pin = body_json(minted).await["pin"].as_str().unwrap().to_string();
    let redeemed = post_json(
        router.clone(),
        "/api/otp/redeem",
        Body::from(json!({"pin": pin}).to_string()),
    )
    .await;
    let session = body_json(redeemed).await["session"]
        .as_str()
        .unwrap()
        .to_string();
    let cookie = format!("streamaid_session={session}");

    let off = router
        .clone()
        .oneshot(
            Request::post("/api/files")
                .header("Cookie", cookie.clone())
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "x.txt", "data": encode_b64(b"x")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    let (st, _) = json_error_body(off).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    app.cfg.lock().control.enabled = true;
    let on = router
        .oneshot(
            Request::post("/api/files")
                .header("Cookie", cookie)
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"name": "x.txt", "data": encode_b64(b"x")}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(on.status(), StatusCode::OK);
}
