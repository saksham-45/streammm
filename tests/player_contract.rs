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
        .and_then(|s| {
            s.chars()
                .take_while(|c| c.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .ok()
        })
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
    assert!(js.contains("mousedown"), "drag starts on mouse down");
    assert!(
        js.contains("contextmenu"),
        "right-click must reach the host"
    );
    assert!(
        js.contains("paste"),
        "viewer clipboard paste must inject on the host"
    );
    assert!(
        js.contains("modifiers") || js.contains("metaKey"),
        "modifier shortcuts must type-through"
    );
    assert!(
        js.contains("image/png"),
        "origin UI must paste PNG clipboard through"
    );
    assert!(
        js.contains("clipboardHasNonImageFiles") && js.contains("clipboardData.files"),
        "origin UI must paste Finder files into the host inbox"
    );
    assert!(
        js.contains("128 * 1024 * 1024")
            && js.contains("pasteImageFile")
            && js.contains("phase: \"begin\"")
            && js.contains("incomingPng.got")
            && js.contains("file.size > CLIP_PNG_MAX"),
        "origin UI must chunk clipboard PNGs up to 128 MB and bound incoming assembly"
    );
    assert!(js.contains("streamaid_viewer") || js.contains("hasViewerSession"));
    assert!(js.contains("/api/computer-use/cancel"));
    assert!(
        js.contains("ctl.controller") && js.contains("/api/control/release"),
        "host UI must reveal End session from status.control.controller"
    );
    assert!(
        js.contains("function currentToken") && js.contains("/api/login"),
        "host UI must read the live token and POST /api/login instead of blindly setting a cookie"
    );
    assert!(
        js.contains("function closeDrawer"),
        "Save/Escape must share a drawer closer"
    );
    assert!(
        js.contains("if (r.applied) closeDrawer()"),
        "successful Settings Save must hide the Configuration drawer"
    );
    assert!(
        js.contains("Escape") && js.contains("closeDrawer()"),
        "Escape must dismiss Settings"
    );
    let html =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/index.html"))
            .unwrap();
    assert!(
        html.contains("id=\"login-error\""),
        "PIN/token unlock must surface an error instead of failing silently"
    );
    assert!(
        html.contains("id=\"session-banner\"") && html.contains("id=\"session-end\""),
        "origin must show a remote-session banner with End"
    );
    assert!(
        html.contains("id=\"perm-banner\"")
            && html.contains("id=\"perm-screen\"")
            && html.contains("id=\"perm-ax\"")
            && html.contains("id=\"perm-input\"")
            && js.contains("function syncPermissionUi")
            && js.contains("/api/permissions/open"),
        "missing Screen Recording, Accessibility, or Input Monitoring must reveal a grant banner"
    );
    assert!(
        html.contains("id=\"keys-bar\"")
            && html.contains("id=\"keys-hint\"")
            && html.contains("Ctrl+Alt+Del")
            && js.contains("function sendCombo")
            && js.contains("function comboPayload")
            && js.contains("bindKeysBar"),
        "enabling remote control must reveal a Send-keys bar for browser-stolen shortcuts"
    );
    assert!(
        html.contains("id=\"chat-section\"")
            && html.contains("id=\"chat-log\"")
            && html.contains("id=\"chat-form\"")
            && html.contains("id=\"chat-hint\"")
            && js.contains("function sendChat")
            && js.contains("function chatPayload")
            && js.contains("chat-history"),
        "enabling remote control must reveal a session chat panel"
    );
    assert!(
        html.contains("id=\"cfg-block-local\"") && html.contains("id=\"block-hint\"") && html.contains("id=\"block-local-fields\""),
        "block-local must be a Settings control revealed when remote control is on"
    );
    assert!(
        html.contains("id=\"cfg-blank-screen\"")
            && html.contains("id=\"blank-screen-fields\"")
            && html.contains("id=\"blank-screen-hint\"")
            && html.contains("brightness")
            && js.contains("ctl.blanking")
            && js.contains("SCREEN BLANKED"),
        "blank-screen must be a Settings control revealed when remote control is on"
    );
    assert!(
        html.contains("id=\"cfg-keep-awake\"")
            && html.contains("id=\"keep-awake-fields\"")
            && html.contains("id=\"keep-awake-hint\""),
        "keep-awake must be a Settings control revealed when remote control or unattended is on"
    );
    assert!(
        html.contains("id=\"wol-fields\"")
            && html.contains("id=\"cfg-wol-mac\"")
            && html.contains("id=\"wol-send\"")
            && html.contains("id=\"wol-copy\"")
            && js.contains("function wolPayload")
            && js.contains("function copyWolMac")
            && js.contains("/api/wol"),
        "unattended/remote control must reveal Wake-on-LAN MAC, Copy MAC, and Send wake packet"
    );
    assert!(
        html.contains("id=\"cfg-lock-on-end\"")
            && html.contains("id=\"lock-on-end-fields\"")
            && html.contains("id=\"lock-on-end-hint\""),
        "lock-on-end must be a Settings control revealed when remote control is on"
    );
    assert!(
        html.contains("id=\"cfg-record-sessions\"")
            && html.contains("id=\"record-fields\"")
            && html.contains("id=\"record-hint\"")
            && html.contains("id=\"recordings-section\"")
            && js.contains("/api/recordings")
            && js.contains("refreshRecordings"),
        "record sessions must be a Settings control that reveals the recordings list"
    );
    assert!(
        html.contains("id=\"stream-quality\"")
            && html.contains("Speed")
            && js.contains("function qualityPayload")
            && js.contains("function sendQuality")
            && js.contains("bindStreamQuality"),
        "host must show a stream quality vs speed picker"
    );
    assert!(
        html.contains("id=\"rec\"")
            && js.contains("function startWatchRecord")
            && js.contains("function watchRecordMime")
            && js.contains("MediaRecorder")
            && js.contains("captureStream"),
        "host live view must record locally via MediaRecorder"
    );
    assert!(
        html.contains("id=\"fs\"")
            && js.contains("function toggleWatchFullscreen")
            && js.contains("function isWatchFullscreen")
            && js.contains("requestFullscreen"),
        "host live view must fullscreen the stream pane"
    );
    assert!(
        html.contains("id=\"analysis-pane\"") && html.contains("class=\"hidden\""),
        "origin analysis/AI/Ask chrome must not sit on the live stream"
    );
    assert!(
        html.contains("id=\"llm-fields\"") && html.contains("id=\"cu-section\""),
        "LLM and AI controls must exist so enabling a setting can reveal them"
    );
    assert!(
        html.contains("id=\"cfg-audio\"") && html.contains("id=\"audio-hint\"") && html.contains("id=\"unmute\"")
            && html.contains("id=\"audio-fields\"") && html.contains("id=\"cfg-audio-device\""),
        "audio must be a Settings checkbox that reveals a device picker and Unmute"
    );
    assert!(
        html.contains("id=\"cfg-voice\"")
            && html.contains("id=\"voice-hint\"")
            && html.contains("id=\"voice-aec-hint\"")
            && html.contains("ducked")
            && html.contains("id=\"talk\"")
            && js.contains("function sendVoice")
            && js.contains("function startTalk")
            && js.contains("function talkMicConstraints")
            && js.contains("function setTalkEchoGate")
            && js.contains("echoCancellation")
            && js.contains("getUserMedia"),
        "allow watcher to talk must reveal a Talk button with AEC and half-duplex mute"
    );
    assert!(
        html.contains("id=\"cfg-unattended\"")
            && html.contains("id=\"unattended-fields\"")
            && html.contains("id=\"unattended-hint\"")
            && html.contains("id=\"cfg-unattended-password\"")
            && js.contains("access")
            && js.contains("password_set"),
        "unattended access must be a Settings checkbox that reveals a password field"
    );
    assert!(
        html.contains("PIN or password") || html.contains("PIN or unattended"),
        "host login must accept the unattended password as well as the PIN"
    );
    assert!(
        js.contains("mp4a.40.2") && js.contains("cfg-audio"),
        "host player must request an AAC SourceBuffer when mic audio is on"
    );
    assert!(
        html.contains("id=\"files-section\"") && html.contains("id=\"file-drop\""),
        "enabling remote control must have a files drop target to reveal"
    );
    assert!(
        js.contains("action: \"begin\"") && js.contains("action: \"chunk\""),
        "origin UI must send large inbox files in chunks"
    );
    assert!(
        js.contains("offset") && js.contains("accept"),
        "origin UI must resume inbox uploads from accept.offset"
    );
    assert!(
        js.contains("2 * 1024 * 1024 * 1024") && js.contains("file.slice"),
        "origin UI must stream inbox drops up to 2 GB via File.slice"
    );
    assert!(
        html.contains("id=\"cfg-display\""),
        "Display picker must be a visible Settings control, not hidden behind Detect"
    );
    assert!(
        html.contains("id=\"display-map\"") && js.contains("layoutDisplayMap"),
        "host must show a clickable all-monitors map"
    );
    assert!(
        js.contains("paintMapThumbs") && js.contains("mon-thumb"),
        "host display map must paint a live thumbnail into the active monitor tile"
    );
    assert!(
        js.contains("loopMapThumbs") && js.contains("requestAnimationFrame"),
        "host active map tile must paint every animation frame, not a 250ms still"
    );
    assert!(
        js.contains("applyDisplayThumbs") && js.contains("\"thumbs\""),
        "host display map must apply JPEG thumbs of unselected monitors"
    );
    assert!(
        js.contains("thumbSeq"),
        "host inactive thumbs must drop stale JPEG decodes so live fps does not paint out of order"
    );
    assert!(
        js.contains("function syncFeatureUi") && js.contains("syncFeatureUi()"),
        "enabling LLM/AI/control must drive a UI reveal helper"
    );
    assert!(
        html.contains("id=\"jpeg-fields\"") && js.contains("function syncEncoderUi"),
        "JPEG quality must hide unless MJPEG is selected"
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
    assert!(
        s.contains("mousedown"),
        "watch page must drag via mouse down/up"
    );
    assert!(
        s.contains("contextmenu"),
        "watch page must send right-click"
    );
    assert!(
        s.contains("paste"),
        "watch page must paste clipboard through"
    );
    assert!(
        s.contains("clipboardHasNonImageFiles") && s.contains("clipboardData.files"),
        "watch page must paste Finder files into the host inbox"
    );
    assert!(
        s.contains("image/png"),
        "watch page must sync PNG clipboard"
    );
    assert!(
        s.contains("128 * 1024 * 1024")
            && s.contains("pasteImageFile")
            && s.contains("phase: \"begin\"")
            && s.contains("incomingPng.got")
            && s.contains("file.size > CLIP_PNG_MAX"),
        "watch page must chunk clipboard PNGs up to 128 MB and bound incoming assembly"
    );
    assert!(
        s.contains("files-section") && s.contains("file-drop"),
        "watch page must reveal file transfer when remote control is on"
    );
    assert!(
        s.contains("keys-bar")
            && s.contains("function sendCombo")
            && s.contains("function comboPayload")
            && s.contains("Ctrl+Alt+Del")
            && s.contains("keys.style.display = controlOn"),
        "watch page must reveal a Send-keys bar when remote control is on"
    );
    assert!(
        s.contains("chat-section")
            && s.contains("function sendChat")
            && s.contains("function chatPayload")
            && s.contains("chat-history")
            && s.contains("chat.style.display = controlOn"),
        "watch page must reveal session chat when remote control is on"
    );
    assert!(
        s.contains("file-offer") && s.contains("msg.offer") && s.contains("showFileOffer"),
        "watch page must reveal a Save control when the host offers a Finder file"
    );
    assert!(
        s.contains("id=\"unmute\"") && s.contains("mp4a.40.2") && s.contains("ctl.audio"),
        "watch page must reveal Unmute and use AAC codecs when the host enables mic audio"
    );
    assert!(
        s.contains("action: \"begin\"") && s.contains("action: \"chunk\""),
        "watch page must send large inbox files in chunks"
    );
    assert!(
        s.contains("offset") && s.contains("accept"),
        "watch page must resume inbox uploads from accept.offset"
    );
    assert!(
        s.contains("2 * 1024 * 1024 * 1024") && s.contains("file.slice"),
        "watch page must stream inbox drops up to 2 GB via File.slice"
    );
    assert!(
        s.contains("blob-begin") && s.contains("new Blob(done.parts)"),
        "watch page must assemble inbox Get from streamed blob parts, not one in-memory copy"
    );
    assert!(
        s.contains("showSaveFilePicker") && s.contains("createWritable"),
        "watch page Get must stream to disk via the file picker when the browser allows it"
    );
    assert!(
        s.contains("/api/files/download"),
        "watch page Get must use HTTP download on browsers without a file picker (Safari)"
    );
    assert!(
        s.contains("displays") && s.contains("type: \"display\""),
        "watch page must switch host displays over the control channel"
    );
    assert!(
        s.contains("display-map") && s.contains("layoutDisplayMap"),
        "watch page must show a TeamViewer-style all-monitors map"
    );
    assert!(
        s.contains("paintMapThumbs") && s.contains("mon-thumb"),
        "watch page display map must paint a live thumbnail into the active monitor tile"
    );
    assert!(
        s.contains("loopMapThumbs") && s.contains("requestAnimationFrame"),
        "watch page active map tile must paint every animation frame, not a 250ms still"
    );
    assert!(
        s.contains("applyDisplayThumbs") && s.contains("\"thumbs\""),
        "watch page display map must apply JPEG thumbs of unselected monitors"
    );
    assert!(
        s.contains("thumbSeq"),
        "watch page inactive thumbs must drop stale JPEG decodes so live fps does not paint out of order"
    );
    assert!(!s.contains("Add ?token="));
    assert!(
        s.contains("CAP = 24") || s.contains("pending.length >= 16"),
        "worker player pending cap of 2 drops init/fragments under jitter"
    );
    assert!(
        s.contains("Max-Age=86400") || s.contains("expires_in_s"),
        "watch page must keep the redeemed session for a day"
    );
    assert!(
        s.contains("PIN or password")
            && s.contains("maxlength=\"128\"")
            && !s.contains("maxlength=\"6\""),
        "watch page must accept an unattended password after the PIN expires"
    );
    assert!(
        s.contains("id=\"stream-quality\"")
            && s.contains("function qualityPayload")
            && s.contains("function sendQuality")
            && s.contains("type: \"quality\""),
        "watch page must send quality presets over the control channel"
    );
    assert!(
        s.contains("id=\"talk\"")
            && s.contains("function sendVoice")
            && s.contains("function startTalk")
            && s.contains("function talkMicConstraints")
            && s.contains("function setTalkEchoGate")
            && s.contains("echoCancellation")
            && s.contains("ctl.voice")
            && s.contains("getUserMedia"),
        "watch page must Talk with AEC and mute the stream while speaking"
    );
    assert!(
        s.contains("id=\"wol-copy\"")
            && s.contains("function fillWolMacs")
            && s.contains("function copyWolMac")
            && s.contains("macs"),
        "watch page must reveal Copy MAC from host flags for Wake-on-LAN"
    );
    assert!(
        s.contains("id=\"rec\"")
            && s.contains("function startWatchRecord")
            && s.contains("function watchRecordMime")
            && s.contains("MediaRecorder")
            && s.contains("captureStream"),
        "watch page must record the live view locally via MediaRecorder"
    );
    assert!(
        s.contains("id=\"fs\"")
            && s.contains("id=\"stage\"")
            && s.contains("function toggleWatchFullscreen")
            && s.contains("requestFullscreen")
            && s.contains("F11"),
        "watch page must fullscreen the live stage (F11 / Full)"
    );
}

#[test]
fn current_token_prefers_query_then_cookie_and_url_attaches_it() {
    let js_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web/app.js");
    let status = Command::new("node")
        .arg("-e")
        .arg(format!(
            r#"
globalThis.window = globalThis;
globalThis.document = {{
  readyState: "loading",
  cookie: "streamaid_token=" + encodeURIComponent("from-cookie"),
  addEventListener: function() {{}},
  getElementById: function() {{ return null; }}
}};
globalThis.location = {{ href: "http://127.0.0.1:8080/?token=from-query", search: "?token=from-query", protocol: "http:", reload: function(){{}} }};
globalThis.navigator = {{ userAgent: "test", platform: "MacIntel", maxTouchPoints: 0 }};
globalThis.WebSocket = function() {{}};
globalThis.MediaSource = undefined;
globalThis.EventSource = undefined;
globalThis.fetch = undefined;
const fs = require("fs");
const src = fs.readFileSync({:?}, "utf8");
(0, eval)(src);
if (!window.streamaidUi || typeof window.streamaidUi.currentToken !== "function") throw new Error("currentToken missing");
if (window.streamaidUi.currentToken() !== "from-query") throw new Error("query token lost: " + window.streamaidUi.currentToken());
location.search = "";
if (window.streamaidUi.currentToken() !== "from-cookie") throw new Error("cookie token lost: " + window.streamaidUi.currentToken());
const u = window.streamaidUi.url("/api/status");
if (u.indexOf("token=from-cookie") < 0) throw new Error("url() must attach token, got " + u);
console.log("token-ok");
"#,
            js_path
        ))
        .status()
        .expect("node");
    assert!(
        status.success(),
        "currentToken/url must drive live query+cookie token"
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
fn display_map_places_secondary_screen_to_the_right() {
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
globalThis.navigator = {{ userAgent: "test" }};
globalThis.WebSocket = function() {{}};
const fs = require("fs");
(0, eval)(fs.readFileSync({:?}, "utf8"));
const layout = window.streamaidUi.layoutDisplayMap([
  {{ id: "2:", x: 0, y: 0, width: 1440, height: 900, main: true }},
  {{ id: "3:", x: 1440, y: 0, width: 1920, height: 1080 }}
], 168, 76);
if (layout.length !== 2) throw new Error("expected 2 monitors");
if (!(layout[1].left > layout[0].left)) throw new Error("secondary must sit to the right");
if (!(layout[1].width > layout[0].width)) throw new Error("wider screen must render wider");
console.log("map-ok");
"#,
            js_path
        ))
        .status()
        .expect("node");
    assert!(
        status.success(),
        "display map layout must place screens by global bounds"
    );
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
    assert!(
        status.success(),
        "drawer open/close must drive shipped helpers"
    );
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
  "block-local-fields": el(true),
  "cfg-block-local": el(false),
  "block-hint": el(true),
  "blank-screen-fields": el(true),
  "cfg-blank-screen": el(false),
  "blank-screen-hint": el(true),
  "lock-on-end-fields": el(true),
  "cfg-lock-on-end": el(false),
  "lock-on-end-hint": el(true),
  "record-fields": el(true),
  "cfg-record-sessions": el(false),
  "record-hint": el(true),
  "recordings-section": el(true),
  "ai-hint": el(true),
  "cu-cancel": el(true),
  "cu-section": el(true),
  "files-section": el(true),
  "keys-bar": el(true),
  "keys-hint": el(true),
  "chat-section": el(true),
  "chat-hint": el(true),
  "analysis-section": el(true),
  "analysis-banner": el(true),
  "analysis-pane": el(true),
  "perm-banner": el(true),
  "perm-banner-label": el(false),
  "perm-screen": el(true),
  "perm-ax": el(true),
  "perm-input": el(true),
  "jpeg-fields": el(true),
  "bitrate-fields": el(false),
  "gop-fields": el(false),
  "cfg-unattended": el(false),
  "unattended-hint": el(true),
  "unattended-fields": el(true),
  "cfg-unattended-password": el(false),
  "keep-awake-fields": el(true),
  "cfg-keep-awake": el(false),
  "keep-awake-hint": el(true),
  "wol-fields": el(true),
  "wol-host-mac": el(false),
  "cfg-wol-mac": el(false),
  "wol-send": el(false),
  "wol-copy": el(true),
  "wol-hint": el(false),
  "cfg-voice": el(false),
  "voice-hint": el(true),
  "voice-aec-hint": el(true),
  "talk": el(true),
  "stream-video": {{ classList: tokenList(false), hidden: false, muted: true, volume: 1 }},
  "cfg-audio": el(false),
  "audio-hint": el(true),
  "audio-fields": el(true),
  "cfg-audio-device": el(false),
  "unmute": el(true),
  "cfg-mode": {{ classList: tokenList(false), hidden: false, checked: false, value: "ffmpeg" }}
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
if (!nodes["unattended-fields"].classList.contains("hidden")) throw new Error("unattended password visible while off");
if (!nodes["keep-awake-fields"].classList.contains("hidden")) throw new Error("keep-awake visible while off");
if (!nodes["wol-fields"].classList.contains("hidden")) throw new Error("WoL visible while off");
if (!nodes["lock-on-end-fields"].classList.contains("hidden")) throw new Error("lock-on-end visible while off");
if (!nodes["blank-screen-fields"].classList.contains("hidden")) throw new Error("blank-screen visible while off");
if (!nodes["record-fields"].classList.contains("hidden")) throw new Error("record-sessions visible while off");
if (!nodes["recordings-section"].classList.contains("hidden")) throw new Error("recordings list visible while off");
if (!nodes["keys-bar"].classList.contains("hidden")) throw new Error("keys-bar visible while off");
if (!nodes["keys-hint"].classList.contains("hidden")) throw new Error("keys-hint visible while off");
if (!nodes["chat-section"].classList.contains("hidden")) throw new Error("chat visible while off");
if (!nodes["chat-hint"].classList.contains("hidden")) throw new Error("chat hint visible while off");
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
if (nodes["files-section"].classList.contains("hidden")) throw new Error("files panel still hidden after control enable");
if (nodes["keys-bar"].classList.contains("hidden")) throw new Error("keys-bar still hidden after control enable");
if (nodes["keys-hint"].classList.contains("hidden")) throw new Error("keys-hint still hidden after control enable");
if (nodes["chat-section"].classList.contains("hidden")) throw new Error("chat still hidden after control enable");
if (nodes["chat-hint"].classList.contains("hidden")) throw new Error("chat hint still hidden after control enable");
const chat = window.streamaidUi.chatPayload("  hello  ");
if (chat.type !== "chat" || chat.text !== "hello") throw new Error("chatPayload");
if (nodes["block-local-fields"].classList.contains("hidden")) throw new Error("block-local still hidden after control enable");
if (nodes["keep-awake-fields"].classList.contains("hidden")) throw new Error("keep-awake still hidden after control enable");
if (nodes["wol-fields"].classList.contains("hidden")) throw new Error("WoL still hidden after control enable");
const wol = window.streamaidUi.wolPayload("  AA:BB:CC:DD:EE:FF  ");
if (wol.mac !== "AA:BB:CC:DD:EE:FF") throw new Error("wolPayload");
window.streamaidUi.fillWolMacs(["aa:bb:cc:dd:ee:ff"]);
if (nodes["wol-copy"].classList.contains("hidden")) throw new Error("Copy MAC hidden when this Mac has an address");
if (window.streamaidUi.copyWolMac() !== "aa:bb:cc:dd:ee:ff") throw new Error("copyWolMac");
window.streamaidUi.fillWolMacs([]);
if (!nodes["wol-copy"].classList.contains("hidden")) throw new Error("Copy MAC visible with no address");
if (!nodes["keep-awake-hint"].classList.contains("hidden")) throw new Error("keep-awake hint visible while off");
nodes["cfg-keep-awake"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["keep-awake-hint"].classList.contains("hidden")) throw new Error("keep-awake hint still hidden after enable");
if (nodes["blank-screen-fields"].classList.contains("hidden")) throw new Error("blank-screen still hidden after control enable");
if (!nodes["blank-screen-hint"].classList.contains("hidden")) throw new Error("blank-screen hint visible while off");
nodes["cfg-blank-screen"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["blank-screen-hint"].classList.contains("hidden")) throw new Error("blank-screen hint still hidden after enable");
if (nodes["lock-on-end-fields"].classList.contains("hidden")) throw new Error("lock-on-end still hidden after control enable");
if (!nodes["lock-on-end-hint"].classList.contains("hidden")) throw new Error("lock-on-end hint visible while off");
nodes["cfg-lock-on-end"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["lock-on-end-hint"].classList.contains("hidden")) throw new Error("lock-on-end hint still hidden after enable");
if (nodes["record-fields"].classList.contains("hidden")) throw new Error("record-sessions still hidden after control enable");
if (!nodes["record-hint"].classList.contains("hidden")) throw new Error("record hint visible while off");
if (!nodes["recordings-section"].classList.contains("hidden")) throw new Error("recordings list visible while record off");
nodes["cfg-record-sessions"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["record-hint"].classList.contains("hidden")) throw new Error("record hint still hidden after enable");
if (nodes["recordings-section"].classList.contains("hidden")) throw new Error("recordings list still hidden after enable");
const qp = window.streamaidUi.qualityPayload("speed");
if (qp.type !== "quality" || qp.preset !== "speed") throw new Error("qualityPayload");
if (typeof window.streamaidUi.watchRecordMime !== "function") throw new Error("watchRecordMime missing");
if (typeof window.streamaidUi.watchRecordMime() !== "string") throw new Error("watchRecordMime");
if (typeof window.streamaidUi.toggleWatchFullscreen !== "function") throw new Error("toggleWatchFullscreen missing");
if (window.streamaidUi.isWatchFullscreen()) throw new Error("fullscreen must start off");
const combo = window.streamaidUi.comboPayload("Tab", ["Meta"]);
if (combo.action !== "key" || combo.key !== "Tab" || combo.modifiers[0] !== "Meta") throw new Error("comboPayload Cmd+Tab");
const cad = window.streamaidUi.comboPayload("Delete", ["Control", "Alt"]);
if (cad.key !== "Delete" || cad.modifiers.indexOf("Control") < 0 || cad.modifiers.indexOf("Alt") < 0) throw new Error("comboPayload Ctrl+Alt+Del");
if (!nodes["block-hint"].classList.contains("hidden")) throw new Error("block hint visible while off");
nodes["cfg-block-local"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["block-hint"].classList.contains("hidden")) throw new Error("block hint still hidden after enable");
if (nodes["analysis-pane"].classList.contains("hidden")) throw new Error("side pane still hidden after control enable");
if (!nodes["jpeg-fields"].classList.contains("hidden")) throw new Error("JPEG quality visible in H.264 mode");
if (nodes["bitrate-fields"].classList.contains("hidden")) throw new Error("bitrate hidden in H.264 mode");
nodes["cfg-unattended"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["unattended-fields"].classList.contains("hidden")) throw new Error("unattended password still hidden after enable");
if (nodes["unattended-hint"].classList.contains("hidden")) throw new Error("unattended hint still hidden after enable");
if (!nodes["voice-hint"].classList.contains("hidden")) throw new Error("voice hint visible while off");
if (!nodes["voice-aec-hint"].classList.contains("hidden")) throw new Error("AEC hint visible while voice off");
if (!nodes["talk"].classList.contains("hidden")) throw new Error("Talk visible while voice off");
nodes["cfg-voice"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["voice-hint"].classList.contains("hidden")) throw new Error("voice hint still hidden after enable");
if (nodes["talk"].classList.contains("hidden")) throw new Error("Talk still hidden after voice enable");
if (!nodes["voice-aec-hint"].classList.contains("hidden")) throw new Error("AEC hint visible without capture audio");
const vp = window.streamaidUi.voicePayload("AQI=", 16000);
if (vp.type !== "voice" || vp.pcm !== "AQI=" || vp.rate !== 16000) throw new Error("voicePayload");
const mic = window.streamaidUi.talkMicConstraints();
if (!mic.audio || !mic.audio.echoCancellation) throw new Error("talkMicConstraints AEC");
nodes["stream-video"].muted = false;
window.streamaidUi.setTalkEchoGate(true);
if (!nodes["stream-video"].muted) throw new Error("Talk must mute the live stream");
window.streamaidUi.setTalkEchoGate(false);
if (nodes["stream-video"].muted) throw new Error("Talk end must restore stream mute");
if (!nodes["audio-hint"].classList.contains("hidden")) throw new Error("audio hint visible while mic off");
if (!nodes["unmute"].classList.contains("hidden")) throw new Error("Unmute visible while mic off");
nodes["cfg-audio"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["audio-hint"].classList.contains("hidden")) throw new Error("audio hint still hidden after mic enable");
if (nodes["audio-fields"].classList.contains("hidden")) throw new Error("audio device picker still hidden after mic enable");
if (nodes["unmute"].classList.contains("hidden")) throw new Error("Unmute still hidden after mic enable");
if (nodes["voice-aec-hint"].classList.contains("hidden")) throw new Error("AEC hint still hidden when talk+mic");
nodes["cfg-mode"].value = "mjpeg";
window.streamaidUi.syncFeatureUi();
if (!nodes["voice-aec-hint"].classList.contains("hidden")) throw new Error("AEC hint visible in MJPEG mode");
if (!nodes["unmute"].classList.contains("hidden")) throw new Error("Unmute still visible in MJPEG mode");
if (!nodes["audio-fields"].classList.contains("hidden")) throw new Error("audio picker still visible in MJPEG mode");
if (nodes["jpeg-fields"].classList.contains("hidden")) throw new Error("JPEG quality still hidden after MJPEG enable");
if (!nodes["bitrate-fields"].classList.contains("hidden")) throw new Error("bitrate still visible in MJPEG mode");
if (!nodes["gop-fields"].classList.contains("hidden")) throw new Error("GOP still visible in MJPEG mode");
if (typeof window.streamaidUi.syncPermissionUi !== "function") throw new Error("syncPermissionUi missing");
window.streamaidUi.syncPermissionUi({{screen: true, accessibility: true, input: true}});
if (!nodes["perm-banner"].classList.contains("hidden")) throw new Error("perm banner visible when granted");
window.streamaidUi.syncPermissionUi({{screen: false, accessibility: true, input: true}});
if (nodes["perm-banner"].classList.contains("hidden")) throw new Error("perm banner hidden when screen denied");
if (nodes["perm-screen"].classList.contains("hidden")) throw new Error("Screen Recording button hidden when denied");
if (!nodes["perm-ax"].classList.contains("hidden")) throw new Error("Accessibility button visible when granted");
window.streamaidUi.syncPermissionUi({{screen: true, accessibility: false, input: true}});
if (nodes["perm-ax"].classList.contains("hidden")) throw new Error("Accessibility button hidden when denied");
nodes["cfg-block-local"].checked = false;
window.streamaidUi.syncPermissionUi({{screen: true, accessibility: true, input: false}});
if (!nodes["perm-input"].classList.contains("hidden")) throw new Error("Input Monitoring visible without block-local");
nodes["cfg-block-local"].checked = true;
window.streamaidUi.syncPermissionUi({{screen: true, accessibility: true, input: false}});
if (nodes["perm-input"].classList.contains("hidden")) throw new Error("Input Monitoring button hidden when block-local needs it");
if (nodes["perm-banner"].classList.contains("hidden")) throw new Error("perm banner hidden when input denied");
nodes["cfg-control-enabled"].checked = false;
nodes["cfg-keep-awake"].checked = false;
nodes["cfg-unattended"].checked = true;
window.streamaidUi.syncFeatureUi();
if (nodes["keep-awake-fields"].classList.contains("hidden")) throw new Error("keep-awake hidden with unattended only");
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
