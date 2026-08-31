//! Player script contract: WebSocket-first, typed frames, ~0.5 s live edge.

use std::path::PathBuf;
use std::process::Command;

fn app_js() -> String {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/app.js");
    std::fs::read_to_string(p).expect("web/app.js")
}

#[test]
fn prefers_websocket_typed_frames_not_2s5_seek() {
    let js = app_js();
    assert!(js.contains("WebSocket"), "must open WebSocket");
    assert!(js.contains("/stream.ws"), "must target /stream.ws");
    assert!(js.contains("TYPE_INIT"), "must branch on type byte");
    assert!(js.contains("TYPE_FRAG"));
    assert!(js.contains("LIVE_EDGE_S"), "must define live-edge constant");
    assert!(
        js.contains("0.45") || js.contains("LIVE_EDGE_S = 0.45"),
        "live edge must be ~0.5s"
    );
    assert!(
        !js.contains("- 2.5") && !js.contains("- 2.5)"),
        "old 2.5s maybeLiveSeek offset must be gone"
    );
    assert!(js.contains("playbackRate"), "catch-up via playbackRate");
    assert!(js.contains("PENDING_CAP"));
    assert!(
        !js.contains("ftyp"),
        "must append by type byte, not sniff ftyp in a TCP chunk"
    );
    assert!(
        !js.contains("/stream.mp4") || js.contains("WebSocket"),
        "WebSocket is the primary path"
    );
    assert!(js.contains("normEvent"), "pointer mapping helper");
    assert!(js.contains("sendControl"), "control JSON sender");
    assert!(js.contains("streamaid_viewer") || js.contains("hasViewerSession"));
    assert!(js.contains("/api/computer-use/cancel"));
    assert!(js.contains("Have AI use this computer") || std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/index.html")
    ).unwrap().contains("Have AI use this computer"));
}

#[test]
fn worker_player_has_pin_unlock_and_ai_box() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/src/player.ts");
    let s = std::fs::read_to_string(p).expect("player.ts");
    assert!(s.contains("pin-form") || s.contains("6-digit") || s.contains("id=\"pin\""));
    assert!(s.contains("Have AI use this computer"));
    assert!(s.contains("normEvent"));
    assert!(!s.contains("Add ?token="));
}

#[test]
fn evals_in_window_context_without_node_globals() {
    let js_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/app.js");
    let status = Command::new("node")
        .arg("-e")
        .arg(format!(
            r#"
globalThis.window = globalThis;
globalThis.document = {{
  readyState: "loading",
  cookie: "",
  addEventListener: function() {{}},
  getElementById: function() {{ return null; }}
}};
globalThis.location = {{ href: "http://127.0.0.1:8080/", search: "", protocol: "http:", reload: function(){{}} }};
globalThis.navigator = {{ userAgent: "test", platform: "MacIntel", maxTouchPoints: 0 }};
globalThis.WebSocket = function() {{}};
globalThis.MediaSource = undefined;
globalThis.EventSource = undefined;
globalThis.fetch = undefined;
const fs = require("fs");
const src = fs.readFileSync({:?}, "utf8");
if (typeof module !== "undefined") {{ /* node has module; script must not use it */ }}
(0, eval)(src);
console.log("ok");
"#,
            js_path
        ))
        .status()
        .expect("node");
    assert!(status.success(), "app.js must eval without throw");
}
