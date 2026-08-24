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
        .oneshot(
            Request::get("/api/config?token=s3cret")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(res.status(), StatusCode::OK);
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
    app.hub
        .publish_init(Bytes::from(init.clone()), 1920, 1080);
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
