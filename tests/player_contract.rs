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
        js.contains("Max-Age=86400") || js.contains("max-age=86400"),
        "host UI session cookie must last a day, not die when the tab closes"
    );
    let cap = js
        .split("PENDING_CAP = ")
        .nth(1)
        .and_then(|s| s.chars().take_while(|c| c.is_ascii_digit()).collect::<String>().parse::<u32>().ok())
        .unwrap_or(0);
    assert!(
        cap >= 16,
        "player must buffer more than two fragments or Cloudflare jitter drops the GOP, PENDING_CAP={cap}"
    );
    assert!(
        js.contains("kind === TYPE_INIT") || js.contains("item.kind === TYPE_INIT"),
        "enqueue/pump must treat INIT as sticky so a full queue cannot drop the fMP4 init"
    );
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
    assert!(js.contains("function closeDrawer"), "Save/Escape must share a drawer closer");
    assert!(
        js.contains("if (r.applied) closeDrawer()"),
        "successful Settings Save must hide the Configuration drawer"
    );
    assert!(
        js.contains("Escape") && js.contains("closeDrawer()"),
        "Escape must dismiss Settings"
    );
    let html = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/index.html"),
    )
    .unwrap();
    assert!(
        html.contains("id=\"analysis-pane\"") && html.contains("class=\"hidden\""),
        "origin analysis/AI/Ask chrome must not sit on the live stream"
    );
    assert!(
        html.contains("id=\"llm-fields\"") && html.contains("id=\"cu-section\""),
        "LLM and AI controls must exist so enabling a setting can reveal them"
    );
    assert!(
        js.contains("function syncFeatureUi") && js.contains("syncFeatureUi()"),
        "enabling LLM/AI/control must drive a UI reveal helper"
    );
    assert!(
        !js.contains("pane.classList.add(\"hidden\")"),
        "loadConfig must not force-hide analysis chrome after the host enables it"
    );
    assert!(
        html.contains("id=\"drawer-backdrop\""),
        "click-outside backdrop dismisses Settings"
    );
    assert!(
        !html.contains("presenter") && !html.contains("screen-share-strip"),
        "host markup must not paint a presenter/share/camera strip"
    );
}

#[test]
fn worker_player_has_pin_unlock_and_ai_box() {
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("cloudflare/src/player.ts");
    let s = std::fs::read_to_string(p).expect("player.ts");
    assert!(s.contains("pin-form") || s.contains("6-digit") || s.contains("id=\"pin\""));
    assert!(s.contains("Have AI use this computer"));
    assert!(s.contains("id=\"cu-section\"") || s.contains("cu-section"));
    assert!(
        s.contains("applyFlags") && s.contains("ai_enabled"),
        "watch page must reveal AI chrome only after host enables AI computer use"
    );
    assert!(s.contains("normEvent"));
    assert!(!s.contains("Add ?token="));
    assert!(
        s.contains("CAP = 24") || s.contains("pending.length >= 16"),
        "worker player pending cap of 2 drops init/fragments under jitter"
    );
    assert!(
        s.contains("Max-Age=86400") || s.contains("expires_in_s"),
        "watch page must keep the redeemed session for a day"
    );
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

#[test]
fn settings_drawer_closes_on_save_helper_and_escape() {
    let js_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/app.js");
    let status = Command::new("node")
        .arg("-e")
        .arg(format!(
            r#"
function tokenList(initialHidden) {{
  const t = {{ hidden: !!initialHidden }};
  return {{
    contains: function (c) {{ return c === "hidden" ? t.hidden : false; }},
    toggle: function (c, force) {{
      if (c !== "hidden") return;
      t.hidden = (force === undefined) ? !t.hidden : !!force;
    }},
    add: function (c) {{ if (c === "hidden") t.hidden = true; }},
    remove: function (c) {{ if (c === "hidden") t.hidden = false; }},
    _t: t
  }};
}}
const drawer = {{ classList: tokenList(true) }};
const backdrop = {{ classList: tokenList(true) }};
globalThis.window = globalThis;
globalThis.document = {{
  readyState: "loading",
  cookie: "",
  addEventListener: function() {{}},
  getElementById: function (id) {{
    if (id === "config-drawer") return drawer;
    if (id === "drawer-backdrop") return backdrop;
    return null;
  }}
}};
globalThis.location = {{ href: "http://127.0.0.1:8080/", search: "", protocol: "http:", reload: function(){{}} }};
globalThis.navigator = {{ userAgent: "test", platform: "MacIntel", maxTouchPoints: 0 }};
globalThis.WebSocket = function() {{}};
globalThis.MediaSource = undefined;
globalThis.EventSource = undefined;
globalThis.fetch = undefined;
const fs = require("fs");
const src = fs.readFileSync({:?}, "utf8");
(0, eval)(src);
if (!window.streamaidUi) throw new Error("streamaidUi missing");
window.streamaidUi.openDrawer();
if (!window.streamaidUi.isDrawerOpen()) throw new Error("open failed");
if (drawer.classList.contains("hidden")) throw new Error("drawer still hidden after open");
if (backdrop.classList.contains("hidden")) throw new Error("backdrop still hidden after open");
window.streamaidUi.closeDrawer();
if (window.streamaidUi.isDrawerOpen()) throw new Error("close failed");
if (!drawer.classList.contains("hidden")) throw new Error("drawer not hidden after close");
if (!backdrop.classList.contains("hidden")) throw new Error("backdrop not hidden after close");
console.log("drawer-ok");
"#,
            js_path
        ))
        .status()
        .expect("node");
    assert!(status.success(), "drawer open/close must drive shipped helpers");
}

#[test]
fn enabling_settings_reveals_their_controls() {
    let js_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/app.js");
    let status = Command::new("node")
        .arg("-e")
        .arg(format!(
            r#"
function tokenList(initialHidden) {{
  const t = {{ hidden: !!initialHidden }};
  return {{
    contains: function (c) {{ return c === "hidden" ? t.hidden : false; }},
    toggle: function (c, force) {{
      if (c !== "hidden") return;
      t.hidden = (force === undefined) ? !t.hidden : !!force;
    }},
    add: function (c) {{ if (c === "hidden") t.hidden = true; }},
    remove: function (c) {{ if (c === "hidden") t.hidden = false; }},
    _t: t
  }};
}}
function el(hidden) {{
  return {{ classList: tokenList(hidden), hidden: !!hidden, checked: false, value: "" }};
}}
const nodes = {{
  "config-drawer": el(true),
  "drawer-backdrop": el(true),
  "cfg-llm-enabled": el(false),
  "cfg-ai-enabled": el(false),
  "cfg-control-enabled": el(false),
  "llm-fields": el(true),
  "ctl-hint": el(true),
  "ai-hint": el(true),
  "cu-cancel": el(true),
  "cu-section": el(true),
  "analysis-section": el(true),
  "analysis-banner": el(true),
  "analysis-pane": el(true)
}};
globalThis.window = globalThis;
globalThis.document = {{
  readyState: "loading",
  cookie: "",
  addEventListener: function() {{}},
  getElementById: function (id) {{ return nodes[id] || null; }}
}};
globalThis.location = {{ href: "http://127.0.0.1:8080/", search: "", protocol: "http:", reload: function(){{}} }};
globalThis.navigator = {{ userAgent: "test", platform: "MacIntel", maxTouchPoints: 0 }};
globalThis.WebSocket = function() {{}};
globalThis.MediaSource = undefined;
globalThis.EventSource = undefined;
globalThis.fetch = undefined;
const fs = require("fs");
const src = fs.readFileSync({:?}, "utf8");
(0, eval)(src);
if (!window.streamaidUi || !window.streamaidUi.syncFeatureUi) throw new Error("syncFeatureUi missing");
window.streamaidUi.syncFeatureUi();
if (!nodes["llm-fields"].classList.contains("hidden")) throw new Error("LLM fields visible while off");
if (!nodes["analysis-pane"].classList.contains("hidden")) throw new Error("analysis pane visible while off");
if (!nodes["cu-section"].classList.contains("hidden")) throw new Error("AI form visible while off");
nodes["cfg-llm-enabled"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["llm-fields"].classList.contains("hidden")) throw new Error("LLM fields still hidden after enable");
if (nodes["analysis-pane"].classList.contains("hidden")) throw new Error("analysis pane still hidden after LLM enable");
if (nodes["analysis-section"].classList.contains("hidden")) throw new Error("analysis section still hidden after LLM enable");
if (!nodes["cu-section"].classList.contains("hidden")) throw new Error("AI form shown without AI enable");
nodes["cfg-llm-enabled"].checked = false;
nodes["cfg-ai-enabled"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["cu-section"].classList.contains("hidden")) throw new Error("AI form still hidden after AI enable");
if (nodes["analysis-pane"].classList.contains("hidden")) throw new Error("analysis pane still hidden after AI enable");
if (!nodes["llm-fields"].classList.contains("hidden")) throw new Error("LLM fields shown without LLM enable");
nodes["cfg-control-enabled"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["ctl-hint"].classList.contains("hidden")) throw new Error("control hint still hidden after enable");
console.log("reveal-ok");
"#,
            js_path
        ))
        .status()
        .expect("node");
    assert!(
        status.success(),
        "enabling LLM/AI/control must reveal that setting's controls"
    );
}
