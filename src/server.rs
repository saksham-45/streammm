//! HTTP + WebSocket origin.

use crate::capture::{enumerate_devices, Capture};
use crate::computer_use::{self, ActionModel};
use crate::config::{self, Config};
use crate::files::Inbox;
use crate::headers::stream_header_map;
use crate::hub::Hub;
use crate::input::{self, FakeInjector, Injector};
use crate::otp::{Clock, OtpGate, RealClock, RedeemError, SESSION_TTL};
use crate::protocol::{pack_media, TYPE_FRAG, TYPE_INIT, TYPE_JPEG, TYPE_SNAP};
use crate::publisher::Publisher;
use crate::ws::{
    accept_key, decode_frame, encode_frame, OP_BIN, OP_CLOSE, OP_PING, OP_PONG, OP_TEXT,
};
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive};
use axum::response::{IntoResponse, Response, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use bytes::Bytes;
use futures_util::StreamExt;
use hyper::upgrade::OnUpgrade;
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio_stream::wrappers::{BroadcastStream, ReceiverStream};

const INDEX: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const STYLE: &str = include_str!("../web/style.css");

pub struct App {
    pub cfg: Mutex<Config>,
    pub cfg_path: PathBuf,
    pub hub: Hub,
    pub capture: Capture,
    pub publisher: Publisher,
    pub started: Instant,
    pub events: tokio::sync::broadcast::Sender<String>,
    pub otp: OtpGate,
    pub injector: Arc<dyn Injector>,
    pub fake: FakeInjector,
    pub model: Mutex<Arc<dyn ActionModel>>,
    pub controller: Mutex<Option<String>>,
    pub ai_cancel: Arc<AtomicBool>,
    pub clip_tx: tokio::sync::broadcast::Sender<String>,
    pub files: Inbox,
}

impl App {
    pub fn new(cfg: Config, cfg_path: PathBuf) -> Arc<Self> {
        let model = computer_use::production_model(&cfg.llm);
        Self::build(
            cfg,
            cfg_path,
            Arc::new(RealClock),
            input::production_injector(),
            FakeInjector::new(),
            model,
        )
    }

    pub fn new_for_test(
        cfg: Config,
        cfg_path: PathBuf,
        clock: Arc<dyn Clock>,
        fake: FakeInjector,
        model: Arc<dyn ActionModel>,
    ) -> Arc<Self> {
        Self::build(cfg, cfg_path, clock, Arc::new(fake.clone()), fake, model)
    }

    fn build(
        cfg: Config,
        cfg_path: PathBuf,
        clock: Arc<dyn Clock>,
        injector: Arc<dyn Injector>,
        fake: FakeInjector,
        model: Arc<dyn ActionModel>,
    ) -> Arc<Self> {
        let hub = Hub::new();
        let capture = Capture::new(hub.clone());
        let (inbound_tx, inbound_rx) = mpsc::channel::<String>(64);
        let publisher = Publisher::new(hub.clone(), cfg.clone(), inbound_tx);
        let (events, _) = tokio::sync::broadcast::channel(128);
        let (clip_tx, _) = tokio::sync::broadcast::channel(16);
        let inbox_dir = cfg_path
            .parent()
            .map(|p| p.join("inbox"))
            .unwrap_or_else(|| PathBuf::from("inbox"));
        let app = Arc::new(Self {
            cfg: Mutex::new(cfg),
            cfg_path,
            hub,
            capture,
            publisher,
            started: Instant::now(),
            events,
            otp: OtpGate::new(clock),
            injector,
            fake,
            model: Mutex::new(model),
            controller: Mutex::new(None),
            ai_cancel: Arc::new(AtomicBool::new(false)),
            clip_tx,
            files: Inbox::new(inbox_dir),
        });
        let app2 = app.clone();
        tokio::spawn(async move {
            let mut rx = inbound_rx;
            while let Some(msg) = rx.recv().await {
                app2.handle_inbound_json(&msg);
            }
        });
        app
    }

    pub fn set_model(&self, model: Arc<dyn ActionModel>) {
        *self.model.lock() = model;
    }

    pub fn push_otp_wire(&self) {
        if let Some((hash, exp)) = self.otp.wire_payload() {
            self.publisher
                .push_wire(json!({"type": "otp", "hash": hash, "exp": exp}).to_string());
        }
        let c = self.cfg.lock().control.clone();
        self.publisher.push_wire(
            json!({"type": "flags", "control": c.enabled, "ai": c.ai_enabled}).to_string(),
        );
    }

    pub fn handle_inbound_json(self: &Arc<Self>, msg: &str) {
        let v: Value = match serde_json::from_str(msg) {
            Ok(v) => v,
            Err(_) => return,
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match ty {
            "control" => {
                if !self.cfg.lock().control.enabled {
                    return;
                }
                let sess = v.get("session").and_then(|s| s.as_str()).unwrap_or("");
                if !self.take_controller(sess) {
                    return;
                }
                if let Some(a) = input::parse_control_json(&v) {
                    self.injector.apply(&a);
                    self.fanout_clipboard_action(&a);
                    if a.requests_clipboard_read() {
                        self.spawn_clipboard_push();
                    }
                }
            }
            "computer-use" => {
                if !self.cfg.lock().control.ai_enabled {
                    return;
                }
                let task = v
                    .get("task")
                    .and_then(|t| t.as_str())
                    .unwrap_or("")
                    .to_string();
                let app = self.clone();
                tokio::spawn(async move {
                    let _ = app.run_computer_use(&task).await;
                });
            }
            "file" => {
                if !self.cfg.lock().control.enabled {
                    return;
                }
                for msg in self.files.handle_message(&v) {
                    self.publisher.push_wire(msg.clone());
                    let _ = self.clip_tx.send(msg);
                }
            }
            "revoke" => {
                *self.controller.lock() = None;
            }
            _ => {}
        }
    }

    fn fanout_clipboard_action(&self, action: &input::Action) {
        let text = match action {
            input::Action::Clipboard { text } | input::Action::Paste { text } => text.clone(),
            _ => return,
        };
        if text.is_empty() {
            return;
        }
        let msg = json!({"type": "clipboard", "text": text}).to_string();
        self.publisher.push_wire(msg.clone());
        let _ = self.clip_tx.send(msg);
    }

    fn spawn_clipboard_push(self: &Arc<Self>) {
        let inj = self.injector.clone();
        let publisher = self.publisher.clone();
        let clip_tx = self.clip_tx.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(120)).await;
            if let Some(text) = inj.clipboard_get() {
                if text.is_empty() {
                    return;
                }
                let msg = json!({"type": "clipboard", "text": text}).to_string();
                publisher.push_wire(msg.clone());
                let _ = clip_tx.send(msg);
            }
        });
    }

    fn take_controller(&self, session: &str) -> bool {
        let mut g = self.controller.lock();
        match g.as_deref() {
            None => {
                if session.is_empty() {
                    *g = Some("anon".into());
                } else {
                    *g = Some(session.to_string());
                }
                true
            }
            Some(cur) if session.is_empty() || cur == session => true,
            Some(_) => false,
        }
    }

    pub async fn run_computer_use(&self, task: &str) -> Vec<input::Action> {
        if !self.cfg.lock().control.ai_enabled {
            return Vec::new();
        }
        self.ai_cancel.store(false, Ordering::SeqCst);
        let model = self.model.lock().clone();
        let hub = self.hub.clone();
        let injector = self.injector.clone();
        computer_use::run_task_async(
            task,
            model.as_ref(),
            injector.as_ref(),
            || computer_use::grab_jpeg(&hub),
            computer_use::MAX_STEPS,
            &self.ai_cancel,
        )
        .await
    }

    pub fn cancel_computer_use(&self) {
        self.ai_cancel.store(true, Ordering::SeqCst);
        *self.controller.lock() = None;
    }

    pub fn start_background(self: &Arc<Self>) {
        let cfg = self.cfg.lock().clone();
        self.capture.start(cfg.clone());
        let _ = self.otp.ensure_current();
        self.push_otp_wire();
        self.publisher.start();
        crate::snapshot::spawn(self.hub.clone());
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                let _ = app.events.send(sse_pack("status", &app.status_json()));
            }
        });
        let app = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                let before = app.otp.current_pin().map(|p| p.hash);
                let st = app.otp.ensure_current();
                if before.as_ref() != Some(&st.hash) {
                    app.push_otp_wire();
                }
            }
        });
    }

    pub fn router(self: Arc<Self>) -> Router {
        Router::new()
            .route("/", get(ui_index))
            .route("/app.js", get(ui_js))
            .route("/style.css", get(ui_css))
            .route("/stream.ws", get(stream_ws))
            .route("/stream.mp4", get(stream_mp4))
            .route("/stream.mjpeg", get(stream_mjpeg))
            .route("/api/status", get(api_status))
            .route("/api/config", get(api_config_get).post(api_config_post))
            .route("/api/events", get(api_events))
            .route("/api/capture-devices", get(api_devices))
            .route("/api/analysis", get(api_analysis))
            .route("/api/ask", post(api_ask))
            .route("/api/analyze-now", post(api_analyze))
            .route("/api/quality", get(api_quality))
            .route("/api/quality-check", post(api_quality_check))
            .route("/api/otp", get(api_otp_get).post(api_otp_mint))
            .route("/api/otp/redeem", post(api_otp_redeem))
            .route("/api/login", post(api_login))
            .route("/api/computer-use", post(api_computer_use))
            .route("/api/computer-use/cancel", post(api_computer_use_cancel))
            .route("/api/files", get(api_files_list).post(api_files_put))
            .route("/api/files/download", get(api_files_download))
            .with_state(self)
    }

    pub fn status_json(&self) -> Value {
        let cfg = self.cfg.lock().clone();
        let cap = self.capture.status();
        let (w, h) = self.hub.size();
        let width = if cap.width > 0 { cap.width } else { w };
        let height = if cap.height > 0 { cap.height } else { h };
        json!({
            "version": env!("CARGO_PKG_VERSION"),
            "uptime_s": self.started.elapsed().as_secs_f64(),
            "capture": {
                "input": if cap.input.is_empty() { cfg.capture.input } else { cap.input },
                "width": width,
                "height": height,
                "fps_target": cfg.capture.fps,
                "fps_actual": self.hub.fps() * cap.frames_per_fragment as f64,
                "running": cap.running,
                "error": cap.error,
                "last_media_age_s": self.hub.last_media_age_s(),
            },
            "stream": {
                "mode": cfg.encoder.mode,
                "clients": self.hub.clients(),
                "bitrate_kbps": cfg.encoder.bitrate_kbps,
                "jpeg_quality": cfg.capture.jpeg_quality,
                "scale": cfg.capture.scale,
                "gop_frames": cfg.encoder.gop_frames,
                "transport": "websocket",
            },
            "llm": {
                "enabled": cfg.llm.enabled,
                "model": cfg.llm.model,
                "interval_sec": cfg.llm.interval_sec,
                "last_run_at": "",
                "last_error": "",
                "active": false,
            },
            "quality": {
                "last_check_at": "",
                "score": 0,
                "sharpness": 0,
                "readability": 0,
                "ocr_confidence": null,
                "ocr_words": 0,
                "ok": false,
                "error": "",
            },
            "control": {
                "enabled": cfg.control.enabled,
                "ai_enabled": cfg.control.ai_enabled,
                "controller": self.controller.lock().is_some(),
                "files": self.files.list().len(),
            },
            "otp": {
                "has_pin": self.otp.current_pin().is_some(),
            }
        })
    }
}

fn sse_pack(name: &str, data: &Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

pub fn token_ok(given: &str, expected: &str) -> bool {
    if expected.is_empty() {
        return true;
    }
    let a = Sha256::digest(given.as_bytes());
    let b = Sha256::digest(expected.as_bytes());
    a.ct_eq(&b).into()
}

fn hex_nibble(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_nibble(bytes[i + 1]), hex_nibble(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn percent_encode_cookie(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn cookie_named(headers: &HeaderMap, name: &str) -> String {
    let Some(c) = headers.get(header::COOKIE).and_then(|v| v.to_str().ok()) else {
        return String::new();
    };
    let prefix = format!("{name}=");
    for part in c.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&prefix) {
            return percent_decode(v);
        }
    }
    String::new()
}

fn request_token(headers: &HeaderMap, query: &str) -> String {
    if let Some(a) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = a.strip_prefix("Bearer ") {
            let rest = rest.trim();
            if !rest.is_empty() {
                return rest.to_string();
            }
        }
    }
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == "token" && !v.is_empty() {
            return v.into_owned();
        }
    }
    cookie_named(headers, "streamaid_token")
}

fn is_public(path: &str) -> bool {
    matches!(
        path,
        "/" | "/app.js" | "/style.css" | "/api/otp/redeem" | "/api/login"
    )
}

fn authorize(app: &App, headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    host_ok(app, headers, uri)
}

fn host_ok(app: &App, headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    let expected = app.cfg.lock().token.clone();
    let query = uri.query().unwrap_or("");
    token_ok(&request_token(headers, query), &expected)
}

fn request_session(headers: &HeaderMap, query: &str) -> String {
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == "session" && !v.is_empty() {
            return v.into_owned();
        }
    }
    let from_cookie = cookie_named(headers, "streamaid_session");
    if !from_cookie.is_empty() {
        return from_cookie;
    }
    if let Some(a) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = a.strip_prefix("Bearer ") {
            return rest.to_string();
        }
    }
    String::new()
}

/// Origin watch/stream: host STREAM_TOKEN (this machine's UI) or PIN session.
/// Empty host token keeps the historical open-dev mode (existing tests).
/// The public Worker watch URL still rejects STREAM_TOKEN (session only).
fn viewer_ok(app: &App, headers: &HeaderMap, uri: &axum::http::Uri) -> bool {
    let expected = app.cfg.lock().token.clone();
    if expected.is_empty() {
        return true;
    }
    if host_ok(app, headers, uri) {
        return true;
    }
    let query = uri.query().unwrap_or("");
    app.otp.session_ok(&request_session(headers, query))
}

fn session_cookie(token: &str) -> HeaderValue {
    let v = format!(
        "streamaid_session={}; Path=/; HttpOnly; SameSite=Lax; Max-Age={}",
        percent_encode_cookie(token),
        SESSION_TTL.as_secs()
    );
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("streamaid_session="))
}

fn token_auth_cookie(token: &str) -> HeaderValue {
    let v = format!(
        "streamaid_token={}; Path=/; SameSite=Lax; Max-Age=86400",
        percent_encode_cookie(token)
    );
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("streamaid_token="))
}

fn viewer_flag_cookie() -> HeaderValue {
    let v = format!(
        "streamaid_viewer=1; Path=/; SameSite=Lax; Max-Age={}",
        SESSION_TTL.as_secs()
    );
    HeaderValue::from_str(&v).unwrap_or_else(|_| {
        HeaderValue::from_static("streamaid_viewer=1; Path=/; SameSite=Lax; Max-Age=86400")
    })
}

fn json_err(code: StatusCode, msg: &str) -> Response {
    (code, Json(json!({"error": msg}))).into_response()
}

async fn ui_index() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], INDEX)
}
async fn ui_js() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "application/javascript")], APP_JS)
}
async fn ui_css() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/css")], STYLE)
}

async fn api_status(State(app): State<Arc<App>>, req: Request) -> Response {
    if !is_public(req.uri().path()) && !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    Json(app.status_json()).into_response()
}

async fn api_config_get(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let cfg = app.cfg.lock().clone();
    Json(cfg).into_response()
}

async fn api_config_post(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let body = axum::body::to_bytes(req.into_body(), 1_000_000)
        .await
        .unwrap_or_default();
    let patch: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    if !patch.is_object() {
        return json_err(StatusCode::BAD_REQUEST, "expected JSON object");
    }
    let old = app.cfg.lock().clone();
    let new_cfg = old.merge_patch(patch);
    let restart_bind =
        old.host != new_cfg.host || old.port != new_cfg.port || old.token != new_cfg.token;
    let recapture = old.capture != new_cfg.capture || old.encoder != new_cfg.encoder;
    if let Err(e) = config::save(&new_cfg, &app.cfg_path) {
        return json_err(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    *app.cfg.lock() = new_cfg.clone();
    app.publisher.set_config(new_cfg.clone());
    if config::cloudflare_endpoint_changed(&old, &new_cfg) {
        app.publisher.start();
    }
    app.push_otp_wire();
    if recapture {
        app.capture.restart(new_cfg);
    }
    let result = json!({
        "applied": true,
        "restart_required": restart_bind,
        "note": if restart_bind { "host/port/token changes take effect on restart" } else { "" }
    });
    let _ = app.events.send(sse_pack("config-applied", &result));
    Json(result).into_response()
}

async fn api_events(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let rx = app.events.subscribe();
    let stream = BroadcastStream::new(rx).filter_map(|msg| async move {
        let Ok(s) = msg else {
            return None;
        };
        let rest = s.strip_prefix("event: ")?;
        let mut parts = rest.splitn(2, "\ndata: ");
        let name = parts.next()?.trim().to_string();
        let data = parts.next().unwrap_or("{}").trim().to_string();
        Some(Ok::<Event, Infallible>(
            Event::default().event(name).data(data),
        ))
    });
    Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response()
}

async fn api_devices(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    Json(enumerate_devices()).into_response()
}

async fn api_analysis(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    Json(json!({
        "last": null,
        "history": [],
        "llm": {
            "configured": false,
            "has_snapshot": false,
            "analyzing": false,
            "note": "Screen analysis and Q&A run on the Cloudflare Worker watch page. Set wrangler secret DEEPSEEK_API_KEY."
        }
    }))
    .into_response()
}

async fn api_ask(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    json_err(
        StatusCode::SERVICE_UNAVAILABLE,
        "Ask the screen on the Cloudflare watch page after DEEPSEEK_API_KEY is set",
    )
}

async fn api_analyze(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    json_err(
        StatusCode::SERVICE_UNAVAILABLE,
        "Analyze-now lives on the Cloudflare Worker",
    )
}

async fn api_quality(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    Json(json!({"last": app.status_json()["quality"], "history": []})).into_response()
}

async fn api_quality_check(State(app): State<Arc<App>>, req: Request) -> Response {
    if !authorize(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    json_err(StatusCode::BAD_GATEWAY, "quality monitor not enabled")
}

fn otp_json(app: &App, st: &crate::otp::PinState) -> Value {
    let ttl = st.exp_unix_ms.saturating_sub(app.otp.unix_ms()) / 1000;
    json!({
        "pin": st.pin,
        "exp": st.exp_unix_ms,
        "expires_in_s": ttl,
        "hash": st.hash,
    })
}

async fn api_otp_get(State(app): State<Arc<App>>, req: Request) -> Response {
    if !host_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let st = match app.otp.current_pin() {
        Some(s) => s,
        None => {
            let s = app.otp.mint();
            app.push_otp_wire();
            s
        }
    };
    Json(otp_json(&app, &st)).into_response()
}

async fn api_otp_mint(State(app): State<Arc<App>>, req: Request) -> Response {
    if !host_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let st = app.otp.mint();
    app.push_otp_wire();
    Json(otp_json(&app, &st)).into_response()
}

async fn api_otp_redeem(State(app): State<Arc<App>>, req: Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), 64_000)
        .await
        .unwrap_or_default();
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    if !v.is_object() {
        return json_err(StatusCode::BAD_REQUEST, "expected JSON object");
    }
    let pin = match v.get("pin") {
        None => return json_err(StatusCode::BAD_REQUEST, "missing pin"),
        Some(p) => match p.as_str() {
            None => return json_err(StatusCode::BAD_REQUEST, "pin must be a string"),
            Some(s) => s.trim(),
        },
    };
    if pin.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing pin");
    }
    match app.otp.redeem(pin) {
        Ok(sess) => {
            let mut res = Json(json!({
                "session": sess.token,
                "expires_in_s": SESSION_TTL.as_secs()
            }))
            .into_response();
            res.headers_mut()
                .append(header::SET_COOKIE, session_cookie(&sess.token));
            res.headers_mut()
                .append(header::SET_COOKIE, viewer_flag_cookie());
            res
        }
        Err(RedeemError::RateLimited) => json_err(StatusCode::TOO_MANY_REQUESTS, "rate limited"),
        Err(RedeemError::Unauthorized) => json_err(StatusCode::UNAUTHORIZED, "unauthorized"),
    }
}

async fn api_login(State(app): State<Arc<App>>, req: Request) -> Response {
    let body = axum::body::to_bytes(req.into_body(), 64_000)
        .await
        .unwrap_or_default();
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    if !v.is_object() {
        return json_err(StatusCode::BAD_REQUEST, "expected JSON object");
    }
    let token = match v.get("token") {
        None => return json_err(StatusCode::BAD_REQUEST, "missing token"),
        Some(p) => match p.as_str() {
            None => return json_err(StatusCode::BAD_REQUEST, "token must be a string"),
            Some(s) => s.trim(),
        },
    };
    if token.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing token");
    }
    let expected = app.cfg.lock().token.clone();
    if !token_ok(token, &expected) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let mut res = Json(json!({ "ok": true })).into_response();
    res.headers_mut()
        .append(header::SET_COOKIE, token_auth_cookie(token));
    res
}

async fn api_computer_use(State(app): State<Arc<App>>, req: Request) -> Response {
    let headers = req.headers().clone();
    let uri = req.uri().clone();
    if !viewer_ok(&app, &headers, &uri) && !host_ok(&app, &headers, &uri) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if !app.cfg.lock().control.ai_enabled {
        return json_err(StatusCode::FORBIDDEN, "ai control disabled");
    }
    let body = axum::body::to_bytes(req.into_body(), 64_000)
        .await
        .unwrap_or_default();
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    let task = v.get("task").and_then(|t| t.as_str()).unwrap_or("").trim();
    if task.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing task");
    }
    let applied = app.run_computer_use(task).await;
    Json(json!({
        "ok": true,
        "steps": applied,
    }))
    .into_response()
}

async fn api_computer_use_cancel(State(app): State<Arc<App>>, req: Request) -> Response {
    if !host_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    app.cancel_computer_use();
    Json(json!({"ok": true, "cancelled": true})).into_response()
}

fn files_ok(app: &App, headers: &HeaderMap, uri: &axum::http::Uri) -> Result<(), Response> {
    if host_ok(app, headers, uri) {
        return Ok(());
    }
    if !viewer_ok(app, headers, uri) {
        return Err(json_err(StatusCode::UNAUTHORIZED, "unauthorized"));
    }
    if !app.cfg.lock().control.enabled {
        return Err(json_err(StatusCode::FORBIDDEN, "remote control disabled"));
    }
    Ok(())
}

async fn api_files_list(State(app): State<Arc<App>>, req: Request) -> Response {
    if let Err(e) = files_ok(&app, req.headers(), req.uri()) {
        return e;
    }
    let files: Vec<Value> = app.files.list().iter().map(|e| e.to_json()).collect();
    Json(json!({"files": files, "dir": app.files.dir.display().to_string()})).into_response()
}

async fn api_files_put(State(app): State<Arc<App>>, req: Request) -> Response {
    if let Err(e) = files_ok(&app, req.headers(), req.uri()) {
        return e;
    }
    let body = axum::body::to_bytes(req.into_body(), crate::files::MAX_FILE * 2)
        .await
        .unwrap_or_default();
    let v: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return json_err(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    if !v.is_object() {
        return json_err(StatusCode::BAD_REQUEST, "expected JSON object");
    }
    let name = v.get("name").and_then(|n| n.as_str()).unwrap_or("").trim();
    if name.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing name");
    }
    let data = match v.get("data").and_then(|d| d.as_str()) {
        Some(s) => match crate::files::decode_b64(s) {
            Ok(b) => b,
            Err(e) => return json_err(StatusCode::BAD_REQUEST, &e),
        },
        None => return json_err(StatusCode::BAD_REQUEST, "missing data"),
    };
    match app.files.put_bytes(name, &data) {
        Ok(ent) => Json(json!({"ok": true, "name": ent.name, "size": ent.size})).into_response(),
        Err(e) => json_err(StatusCode::BAD_REQUEST, &e),
    }
}

async fn api_files_download(State(app): State<Arc<App>>, req: Request) -> Response {
    if let Err(e) = files_ok(&app, req.headers(), req.uri()) {
        return e;
    }
    let query = req.uri().query().unwrap_or("");
    let mut name = String::new();
    for (k, v) in url::form_urlencoded::parse(query.as_bytes()) {
        if k == "name" {
            name = v.into_owned();
        }
    }
    if name.is_empty() {
        return json_err(StatusCode::BAD_REQUEST, "missing name");
    }
    match app.files.get_bytes(&name) {
        Ok(bytes) => {
            let disp = format!("attachment; filename=\"{}\"", name.replace('"', ""));
            let mut res = Response::new(Body::from(bytes));
            res.headers_mut().insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/octet-stream"),
            );
            if let Ok(v) = HeaderValue::from_str(&disp) {
                res.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            res
        }
        Err(e) => json_err(StatusCode::NOT_FOUND, &e),
    }
}

fn media_response(ctype: &str, rx: mpsc::Receiver<Result<Bytes, std::io::Error>>) -> Response {
    let mut b = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, ctype);
    for (k, v) in stream_header_map() {
        b = b.header(k, v);
    }
    let stream = ReceiverStream::new(rx);
    b.body(Body::from_stream(stream)).unwrap()
}

async fn stream_mp4(State(app): State<Arc<App>>, req: Request) -> Response {
    if !viewer_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if app.cfg.lock().encoder.mode == "mjpeg" {
        return json_err(
            StatusCode::CONFLICT,
            "encoder mode is mjpeg; use /stream.mjpeg",
        );
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(32);
    let hub = app.hub.clone();
    let capture = app.capture.clone();
    tokio::spawn(async move {
        if let Some(init) = hub.init_segment() {
            let _ = tx.send(Ok(init)).await;
        }
        let sub = hub.subscribe(32);
        let gen = hub.generation();
        loop {
            tokio::select! {
                m = sub.recv() => {
                    let Some(m) = m else { break; };
                    if hub.generation() != gen { break; }
                    if (m.kind == TYPE_INIT || m.kind == TYPE_FRAG)
                        && tx.send(Ok(m.data)).await.is_err()
                    {
                        break;
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    if !capture.status().running || hub.generation() != gen {
                        break;
                    }
                }
            }
        }
    });
    media_response("video/mp4", rx)
}

fn mjpeg_part(jpeg: &[u8]) -> Bytes {
    let mut v = format!(
        "--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n",
        jpeg.len()
    )
    .into_bytes();
    v.extend_from_slice(jpeg);
    v.extend_from_slice(b"\r\n");
    Bytes::from(v)
}

async fn stream_mjpeg(State(app): State<Arc<App>>, req: Request) -> Response {
    if !viewer_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    if app.cfg.lock().encoder.mode != "mjpeg" {
        return json_err(
            StatusCode::CONFLICT,
            "encoder mode is not mjpeg; use /stream.mp4",
        );
    }
    let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(4);
    let hub = app.hub.clone();
    let capture = app.capture.clone();
    tokio::spawn(async move {
        if let Some(lat) = hub.latest() {
            if lat.kind == TYPE_JPEG {
                let _ = tx.send(Ok(mjpeg_part(&lat.data))).await;
            }
        }
        let sub = hub.subscribe(2);
        loop {
            tokio::select! {
                m = sub.recv() => {
                    let Some(m) = m else { break; };
                    if m.kind == TYPE_JPEG && tx.send(Ok(mjpeg_part(&m.data))).await.is_err() {
                        break;
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {
                    if !capture.status().running { break; }
                }
            }
        }
    });
    media_response("multipart/x-mixed-replace; boundary=frame", rx)
}

async fn stream_ws(State(app): State<Arc<App>>, mut req: Request) -> Response {
    if !viewer_ok(&app, req.headers(), req.uri()) {
        return json_err(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let upgrade = req
        .headers()
        .get(header::UPGRADE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false);
    if !upgrade {
        return json_err(StatusCode::UPGRADE_REQUIRED, "expected websocket");
    }
    let key = match req
        .headers()
        .get("Sec-WebSocket-Key")
        .and_then(|v| v.to_str().ok())
    {
        Some(k) => k.to_string(),
        None => return json_err(StatusCode::BAD_REQUEST, "missing Sec-WebSocket-Key"),
    };
    let accept = accept_key(&key);
    let session = request_session(req.headers(), req.uri().query().unwrap_or(""));
    let on_upgrade = req.extensions_mut().remove::<OnUpgrade>();
    let app2 = app.clone();
    if let Some(on_upgrade) = on_upgrade {
        tokio::spawn(async move {
            match on_upgrade.await {
                Ok(upgraded) => {
                    let io = TokioIo::new(upgraded);
                    if let Err(e) = ws_session(io, app2, session).await {
                        tracing::debug!("ws session: {e}");
                    }
                }
                Err(e) => tracing::warn!("websocket upgrade: {e}"),
            }
        });
    } else {
        return json_err(StatusCode::BAD_REQUEST, "upgrade not available");
    }
    let mut res = Response::new(Body::empty());
    *res.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = res.headers_mut();
    h.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    h.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(
        "Sec-WebSocket-Accept",
        HeaderValue::from_str(&accept).unwrap_or(HeaderValue::from_static("")),
    );
    res
}

async fn ws_session<S>(mut stream: S, app: Arc<App>, session: String) -> anyhow::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let mut read_buf = Vec::new();
    let mut tmp = [0u8; 8192];
    async fn send_bin<S: tokio::io::AsyncWrite + Unpin>(
        stream: &mut S,
        payload: &[u8],
    ) -> std::io::Result<()> {
        let frame = encode_frame(payload, OP_BIN, false);
        stream.write_all(&frame).await?;
        stream.flush().await
    }
    async fn send_txt<S: tokio::io::AsyncWrite + Unpin>(
        stream: &mut S,
        payload: &str,
    ) -> std::io::Result<()> {
        let frame = encode_frame(payload.as_bytes(), OP_TEXT, false);
        stream.write_all(&frame).await?;
        stream.flush().await
    }

    if let Some(init) = app.hub.init_segment() {
        send_bin(&mut stream, &pack_media(TYPE_INIT, &init)).await?;
    }
    if let Some(lat) = app.hub.latest() {
        if lat.kind == TYPE_JPEG {
            send_bin(&mut stream, &pack_media(TYPE_JPEG, &lat.data)).await?;
        } else if lat.kind == TYPE_FRAG && app.hub.init_segment().is_some() {
            send_bin(&mut stream, &pack_media(TYPE_FRAG, &lat.data)).await?;
        }
    }
    let sub = app.hub.subscribe(8);
    let mut clip_rx = app.clip_tx.subscribe();
    loop {
        tokio::select! {
            media = sub.recv() => {
                let Some(m) = media else { break; };
                if m.kind == TYPE_SNAP {
                    continue;
                }
                send_bin(&mut stream, &pack_media(m.kind, &m.data)).await?;
            }
            clip = clip_rx.recv() => {
                if let Ok(msg) = clip {
                    send_txt(&mut stream, &msg).await?;
                }
            }
            n = stream.read(&mut tmp) => {
                let n = n?;
                if n == 0 { break; }
                read_buf.extend_from_slice(&tmp[..n]);
                loop {
                    let (op, data, consumed) = decode_frame(&read_buf);
                    let Some(op) = op else { break; };
                    read_buf.drain(..consumed);
                    match op {
                        OP_PING => {
                            let frame = encode_frame(&data, OP_PONG, false);
                            stream.write_all(&frame).await?;
                        }
                        OP_CLOSE => return Ok(()),
                        OP_TEXT | OP_BIN => {
                            if let Ok(s) = std::str::from_utf8(&data) {
                                if let Ok(mut v) = serde_json::from_str::<Value>(s) {
                                    if v.get("session").is_none() {
                                        v.as_object_mut().map(|o| {
                                            o.insert("session".into(), json!(session));
                                        });
                                    }
                                    app.handle_inbound_json(&v.to_string());
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_expected_token_allows_any() {
        assert!(token_ok("anything", ""));
        assert!(!token_ok("a", "b"));
        assert!(token_ok("secret", "secret"));
    }

    #[test]
    fn percent_decode_matches_js_encode_uri_component() {
        assert_eq!(percent_decode("s3cret"), "s3cret");
        assert_eq!(percent_decode("p%40ss%2Fw%20d%2B"), "p@ss/w d+");
        assert_eq!(percent_encode_cookie("p@ss/w d+"), "p%40ss%2Fw%20d%2B");
    }

    #[test]
    fn origin_does_not_push_encoder_hub_size_into_hid_mapping() {
        let src = include_str!("server.rs");
        let forbidden_fn = format!("fn refresh_{}", "injector_size");
        let forbidden_call = format!("injector.set_{}", "screen_size");
        assert!(
            !src.contains(&forbidden_fn),
            "hub.size() must not overwrite CGDisplayBounds before inject"
        );
        assert!(
            !src.contains(&forbidden_call),
            "production inject path must not set encoder/hub size on the injector"
        );
    }

    #[test]
    fn stream_headers_on_media_response() {
        let (tx, rx) = mpsc::channel::<Result<Bytes, std::io::Error>>(1);
        drop(tx);
        let res = media_response("video/mp4", rx);
        let h = res.headers();
        let cc = h.get("Cache-Control").unwrap().to_str().unwrap();
        assert!(cc.contains("no-store"));
        assert!(cc.contains("no-transform"));
        assert_eq!(h.get("Transfer-Encoding").unwrap(), "chunked");
        assert_eq!(h.get("Content-Type").unwrap(), "video/mp4");
    }

    #[tokio::test]
    async fn origin_serves_ui_config_and_status() {
        use crate::config::Config;
        use tower::ServiceExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = Config::default();
        crate::config::save(&cfg, &path).unwrap();
        let app = App::new(cfg, path);
        let router = app.router();

        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let html = axum::body::to_bytes(res.into_body(), 200_000)
            .await
            .unwrap();
        let html = String::from_utf8_lossy(&html);
        assert!(html.contains("streamaid"));
        assert!(html.contains("app.js"));
        assert!(html.contains("pin-pill"));
        assert!(html.contains("cfg-control-enabled"));
        assert!(html.contains("cfg-ai-enabled"));
        assert!(html.contains("cfg-llm-enabled"));
        assert!(html.contains("id=\"llm-fields\""));
        assert!(html.contains("Have AI use this computer"));
        assert!(html.contains("cu-cancel"));
        assert!(html.contains("id=\"login-overlay\""));
        assert!(html.contains("id=\"login-token\""));
        assert!(html.contains("id=\"login-error\""));

        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 200_000)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(v["encoder"]["mode"], "ffmpeg");
        assert_eq!(v["encoder"]["bitrate_kbps"], 20000);
        assert_eq!(v["encoder"]["gop_frames"], 15);
        assert_eq!(v["encoder"]["max_width"], 3840);
        assert_eq!(v["encoder"]["max_height"], 4320);

        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/api/status")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), 200_000)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&body).unwrap();
        assert!(v.get("capture").is_some());
        assert!(v.get("stream").is_some());

        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .uri("/app.js")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let js = axum::body::to_bytes(res.into_body(), 400_000)
            .await
            .unwrap();
        let js = String::from_utf8_lossy(&js);
        assert!(js.contains("WebSocket"));
        assert!(js.contains("/stream.ws"));
        assert!(js.contains("LIVE_EDGE_S"));
        assert!(js.contains("mousedown"));
        assert!(js.contains("contextmenu"));
        assert!(js.contains("paste"));
        assert!(!js.contains("ftyp"));
    }

    #[tokio::test]
    async fn websocket_delivers_typed_init_then_fragment() {
        use crate::protocol::{unpack_media, TYPE_FRAG, TYPE_INIT};
        use futures_util::StreamExt;
        use tokio::net::TcpListener;
        use tokio_tungstenite::connect_async;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let cfg = crate::config::Config::default();
        crate::config::save(&cfg, &path).unwrap();
        let app = App::new(cfg, path);
        app.hub
            .publish_init(Bytes::from_static(b"ftyp-init"), 1920, 1080);
        app.hub
            .publish_unit(TYPE_FRAG, Bytes::from_static(b"moof-frag"), 0, 0);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });

        let url = format!("ws://{addr}/stream.ws");
        let (mut ws, resp) = connect_async(&url).await.expect("websocket handshake");
        assert_eq!(resp.status(), axum::http::StatusCode::SWITCHING_PROTOCOLS);

        let mut kinds = Vec::new();
        for _ in 0..2 {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next())
                .await
                .expect("timed out waiting for media")
                .expect("socket closed")
                .expect("ws error");
            let bin = msg.into_data();
            let (k, rest) = unpack_media(&bin).expect("envelope");
            kinds.push(k);
            assert!(!rest.is_empty());
        }
        assert_eq!(kinds, vec![TYPE_INIT, TYPE_FRAG]);
    }

    #[tokio::test]
    async fn authorized_viewer_control_reaches_fake_injector() {
        use crate::computer_use::DoneModel;
        use crate::input::{FakeInjector, Injected};
        use crate::otp::FakeClock;
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = crate::config::Config::default();
        cfg.token = "s3cret".into();
        cfg.control.enabled = true;
        crate::config::save(&cfg, &path).unwrap();
        let fake = FakeInjector::new();
        let app = App::new_for_test(
            cfg,
            path,
            FakeClock::new(),
            fake.clone(),
            Arc::new(DoneModel),
        );
        app.hub
            .publish_init(Bytes::from_static(b"ftyp-init"), 1920, 1080);

        let pin = app.otp.mint().pin;
        let sess = app.otp.redeem(&pin).unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });

        let url = format!("ws://{addr}/stream.ws?session={}", sess.token);
        let (mut ws, resp) = connect_async(&url).await.expect("ws");
        assert_eq!(resp.status(), axum::http::StatusCode::SWITCHING_PROTOCOLS);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;

        ws.send(Message::Text(
            r#"{"type":"control","action":"click","x":0.5,"y":0.25}"#.into(),
        ))
        .await
        .unwrap();
        ws.send(Message::Text(
            r#"{"type":"control","action":"type","text":"hi"}"#.into(),
        ))
        .await
        .unwrap();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if fake.recorded().len() >= 2 || tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        assert_eq!(
            fake.recorded(),
            vec![
                Injected::click(0.5, 0.25),
                Injected::Type { text: "hi".into() }
            ]
        );
    }

    #[tokio::test]
    async fn viewer_right_click_drag_paste_and_clipboard_sync() {
        use crate::computer_use::DoneModel;
        use crate::input::{FakeInjector, Injected, MouseButton};
        use crate::otp::FakeClock;
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = crate::config::Config::default();
        cfg.token = "s3cret".into();
        cfg.control.enabled = true;
        crate::config::save(&cfg, &path).unwrap();
        let fake = FakeInjector::new();
        let app = App::new_for_test(
            cfg,
            path,
            FakeClock::new(),
            fake.clone(),
            Arc::new(DoneModel),
        );
        app.hub
            .publish_init(Bytes::from_static(b"ftyp-init"), 1920, 1080);
        let pin = app.otp.mint().pin;
        let sess = app.otp.redeem(&pin).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        let url = format!("ws://{addr}/stream.ws?session={}", sess.token);
        let (mut ws, _) = connect_async(&url).await.expect("ws");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;

        for payload in [
            r#"{"type":"control","action":"click","x":0.2,"y":0.3,"button":"right"}"#,
            r#"{"type":"control","action":"down","x":0.1,"y":0.1,"button":0}"#,
            r#"{"type":"control","action":"move","x":0.8,"y":0.8,"button":0}"#,
            r#"{"type":"control","action":"up","x":0.8,"y":0.8,"button":0}"#,
            r#"{"type":"control","action":"keydown","key":"c","modifiers":["Meta"]}"#,
            r#"{"type":"control","action":"paste","text":"from-viewer"}"#,
        ] {
            ws.send(Message::Text(payload.into())).await.unwrap();
        }

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if fake.recorded().len() >= 6 || tokio::time::Instant::now() > deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        let rec = fake.recorded();
        assert!(
            rec.iter().any(|e| matches!(
                e,
                Injected::Click {
                    button: MouseButton::Right,
                    ..
                }
            )),
            "right-click must inject, got {rec:?}"
        );
        assert!(rec.iter().any(|e| matches!(e, Injected::Down { .. })));
        assert!(rec.iter().any(|e| matches!(e, Injected::Move { .. })));
        assert!(rec.iter().any(|e| matches!(e, Injected::Up { .. })));
        assert!(rec.iter().any(|e| matches!(
            e,
            Injected::Key { key, modifiers, .. }
                if key == "c" && modifiers.iter().any(|m| m == "Meta")
        )));
        assert!(rec.iter().any(|e| matches!(
            e,
            Injected::Paste { text } if text == "from-viewer"
        )));
        assert_eq!(fake.clipboard_get().as_deref(), Some("from-viewer"));

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        let mut got_clip = false;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if t.contains("from-viewer") && t.contains("clipboard") {
                        got_clip = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(
            got_clip,
            "origin must fan clipboard JSON back to the watcher"
        );
    }

    #[tokio::test]
    async fn viewer_file_put_lists_and_gets_over_websocket() {
        use crate::computer_use::DoneModel;
        use crate::files::encode_b64;
        use crate::input::FakeInjector;
        use crate::otp::FakeClock;
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = crate::config::Config::default();
        cfg.token = "s3cret".into();
        cfg.control.enabled = true;
        crate::config::save(&cfg, &path).unwrap();
        let app = App::new_for_test(
            cfg,
            path,
            FakeClock::new(),
            FakeInjector::new(),
            Arc::new(DoneModel),
        );
        app.hub
            .publish_init(Bytes::from_static(b"ftyp-init"), 1920, 1080);
        let pin = app.otp.mint().pin;
        let sess = app.otp.redeem(&pin).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        let url = format!("ws://{addr}/stream.ws?session={}", sess.token);
        let (mut ws, _) = connect_async(&url).await.expect("ws");
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await;
        let payload = encode_b64(b"inbox-bytes");
        ws.send(Message::Text(
            format!(
                r#"{{"type":"file","action":"put","name":"note.txt","data":"{payload}"}}"#
            )
            .into(),
        ))
        .await
        .unwrap();
        let mut saw_ok = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if t.contains("note.txt") && t.contains("\"ok\"") {
                        saw_ok = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_ok, "put must ack over the watch socket");
        ws.send(Message::Text(r#"{"type":"file","action":"list"}"#.into()))
            .await
            .unwrap();
        ws.send(Message::Text(
            r#"{"type":"file","action":"get","name":"note.txt"}"#.into(),
        ))
        .await
        .unwrap();
        let mut saw_list = false;
        let mut saw_blob = false;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline && !(saw_list && saw_blob) {
            match tokio::time::timeout(std::time::Duration::from_millis(200), ws.next()).await {
                Ok(Some(Ok(Message::Text(t)))) => {
                    if t.contains("note.txt") && t.contains("list") {
                        saw_list = true;
                    }
                    if t.contains("blob") && t.contains(&payload) {
                        saw_blob = true;
                    }
                }
                _ => {}
            }
        }
        assert!(saw_list, "list must include the uploaded file");
        assert!(saw_blob, "get must return the file bytes");
    }

    #[tokio::test]
    async fn disabled_control_does_not_inject() {
        use crate::computer_use::DoneModel;
        use crate::input::FakeInjector;
        use crate::otp::FakeClock;
        use futures_util::{SinkExt, StreamExt};
        use tokio::net::TcpListener;
        use tokio_tungstenite::connect_async;
        use tokio_tungstenite::tungstenite::Message;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut cfg = crate::config::Config::default();
        cfg.token = "s3cret".into();
        crate::config::save(&cfg, &path).unwrap();
        let fake = FakeInjector::new();
        let app = App::new_for_test(
            cfg,
            path,
            FakeClock::new(),
            fake.clone(),
            Arc::new(DoneModel),
        );
        let pin = app.otp.mint().pin;
        let sess = app.otp.redeem(&pin).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = app.router();
        tokio::spawn(async move {
            axum::serve(listener, router).await.ok();
        });
        let url = format!("ws://{addr}/stream.ws?session={}", sess.token);
        let (mut ws, _) = connect_async(&url).await.expect("ws");
        let _ = tokio::time::timeout(std::time::Duration::from_millis(500), ws.next()).await;
        ws.send(Message::Text(
            r#"{"type":"control","action":"click","x":0.5,"y":0.5}"#.into(),
        ))
        .await
        .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        assert!(fake.recorded().is_empty());
    }
}
