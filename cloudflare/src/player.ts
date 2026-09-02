/** Public watch page served at GET /. Token is taken from ?token=. */

export const PLAYER_HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>streamaid live</title>
<style>
  :root { color-scheme: dark; --bg:#0d0d0d; --panel:#161616; --line:#2c2c2c; --text:#e8e8e8; --muted:#9a9a9a; --accent:#7eb8ff; }
  * { box-sizing: border-box; }
  body { margin: 0; background: var(--bg); color: var(--text); font-family: ui-sans-serif, system-ui, sans-serif; }
  header { padding: 10px 16px; display: flex; gap: 12px; align-items: center; border-bottom: 1px solid var(--line); flex-wrap: wrap; }
  #displays { display: none; gap: 6px; align-items: center; }
  #displays button { padding: 4px 8px; font-size: 12px; border-radius: 999px; }
  #displays button.on { border-color: var(--accent); color: var(--accent); }
  h1 { font-size: 15px; margin: 0; letter-spacing: 0.14em; font-weight: 650; text-transform: uppercase; }
  #pill { font-size: 12px; border: 1px solid var(--line); border-radius: 999px; padding: 4px 10px; color: var(--muted); }
  main { display: grid; grid-template-columns: minmax(0, 3fr) minmax(300px, 1fr); gap: 12px; padding: 12px; }
  @media (max-width: 800px) { main { grid-template-columns: 1fr; } }
  video, canvas { width: 100%; max-height: 80vh; background: #000; display: block; border-radius: 8px; }
  canvas { display: none; }
  #err { color: #f88; padding: 8px 0; font-size: 13px; }
  aside { background: var(--panel); border: 1px solid var(--line); border-radius: 10px; padding: 14px; display: flex; flex-direction: column; min-height: 0; }
  aside h2 { font-size: 13px; margin: 0 0 6px; letter-spacing: 0.08em; text-transform: uppercase; color: var(--muted); }
  #llm-note { font-size: 12px; color: var(--muted); margin-bottom: 10px; line-height: 1.4; }
  #summary { font-size: 14px; line-height: 1.45; margin-bottom: 10px; }
  .qa { border: 1px solid var(--line); background: #111; border-radius: 8px; padding: 8px 10px; margin: 8px 0; }
  .q { font-weight: 600; }
  .a { margin: 4px 0 2px; }
  .meta { font-size: 12px; color: var(--muted); }
  #ask-form { display: flex; gap: 6px; margin: 10px 0 8px; }
  #ask-form input { flex: 1; background: #111; color: var(--text); border: 1px solid var(--line); border-radius: 6px; padding: 8px; font: inherit; }
  button { background: #222; color: var(--text); border: 1px solid #555; border-radius: 6px; padding: 8px 10px; cursor: pointer; font: inherit; }
  button:hover { border-color: var(--accent); }
  #ask-out { font-size: 13px; color: var(--muted); min-height: 1.2em; }
  #history { list-style: none; padding: 0; margin: 10px 0 0; font-size: 12px; color: var(--muted); overflow: auto; max-height: 28vh; }
  #history li { border-top: 1px solid var(--line); padding: 6px 0; }
  #tap {
    display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.55);
    color: #fff; font-size: 18px; align-items: center; justify-content: center;
  }
  #gate {
    position: fixed; inset: 0; background: rgba(8,8,8,0.92); display: flex;
    align-items: center; justify-content: center; z-index: 20;
  }
  #gate form { display: flex; gap: 8px; }
  #gate input { font-size: 22px; letter-spacing: 0.4em; width: 9em; text-align: center;
    background: #111; color: #fff; border: 1px solid #444; border-radius: 8px; padding: 10px; }
  #cu-form { display: flex; gap: 6px; margin: 8px 0; }
  #cu-form input { flex: 1; background: #111; color: var(--text); border: 1px solid var(--line); border-radius: 6px; padding: 8px; font: inherit; }
  #cu-out { font-size: 12px; color: var(--muted); min-height: 1.2em; }
  #file-drop { border: 1px dashed #555; border-radius: 8px; padding: 10px; font-size: 13px; color: var(--muted); text-align: center; margin: 8px 0; }
  #file-drop.drop-hover { border-color: var(--accent); }
  .file-pick { color: var(--accent); cursor: pointer; }
  .file-pick input { display: none; }
  #file-list { list-style: none; padding: 0; margin: 8px 0 0; font-size: 13px; }
  #file-list li { border-top: 1px solid var(--line); padding: 6px 0; }
  #file-list button { padding: 2px 8px; margin-left: 8px; }
  #file-out { font-size: 12px; color: var(--muted); min-height: 1.2em; }
</style>
</head>
<body>
<header>
  <h1>streamaid</h1>
  <div id="pill">enter PIN</div>
  <div id="displays"></div>
</header>
<div id="gate">
  <form id="pin-form">
    <input id="pin" inputmode="numeric" pattern="[0-9]*" maxlength="6" placeholder="PIN" autocomplete="one-time-code">
    <button type="submit">Unlock</button>
  </form>
</div>
<main>
  <section>
    <video id="v" autoplay muted playsinline webkit-playsinline></video>
    <canvas id="c"></canvas>
    <div id="err"></div>
  </section>
  <aside>
    <div id="cu-section" style="display:none">
    <h2>Have AI use this computer</h2>
    <form id="cu-form">
      <input id="cu-task" placeholder="Task for the host computer…" autocomplete="off">
      <button type="submit">Run</button>
    </form>
    <div id="cu-out"></div>
    </div>
    <div id="files-section" style="display:none">
    <h2>Files</h2>
    <div id="file-drop">Drop files here or <label class="file-pick">browse<input id="file-input" type="file" multiple></label></div>
    <div id="file-out"></div>
    <ul id="file-list"></ul>
    </div>
    <h2>Screen analysis</h2>
    <div id="llm-note">Waiting for DeepSeek key and a screenshot…</div>
    <div id="summary">No analysis yet.</div>
    <div id="qs"></div>
    <form id="ask-form">
      <input id="ask-input" placeholder="Ask about the screen…" autocomplete="off">
      <button type="submit">Ask</button>
    </form>
    <button id="analyze" type="button">Analyze now</button>
    <div id="ask-out"></div>
    <ul id="history"></ul>
  </aside>
</main>
<div id="tap">Tap to play</div>
<script>
(function () {
  var TYPE_INIT = 1, TYPE_FRAG = 2, TYPE_JPEG = 3, TYPE_SNAP = 4;
  var LIVE = 0.45, CAP = 24;
  var session = "";
  var controlOn = false;
  var aiOn = false;
  var pill = document.getElementById("pill");
  var err = document.getElementById("err");
  var video = document.getElementById("v");
  var canvas = document.getElementById("c");
  var tap = document.getElementById("tap");
  video.setAttribute("playsinline", "");
  video.setAttribute("webkit-playsinline", "true");
  video.playsInline = true;
  video.muted = true;
  video.disableRemotePlayback = true;

  function mediaSourceCtor() {
    return window.ManagedMediaSource || window.MediaSource || window.WebKitMediaSource || null;
  }
  function isTypeSupported(MS, t) {
    try { return !!(MS && MS.isTypeSupported && MS.isTypeSupported(t)); } catch (e) { return false; }
  }

  var MS = mediaSourceCtor();
  var ms, sb, ws, pending = [], liveSeeked = false, jpegMode = false, reconnectTimer = null;

  function wsUrl() {
    var u = new URL("/watch", location.href);
    u.protocol = u.protocol.replace("http", "ws");
    u.searchParams.set("session", session);
    return u.toString();
  }
  function sendControl(action, extra) {
    if (!controlOn || !ws || ws.readyState !== 1) return;
    var msg = { type: "control", action: action };
    if (extra) Object.keys(extra).forEach(function (k) { msg[k] = extra[k]; });
    try { ws.send(JSON.stringify(msg)); } catch (e) {}
  }
  function normEvent(el, ev) {
    var r = el.getBoundingClientRect();
    var x = r.width ? (ev.clientX - r.left) / r.width : 0;
    var y = r.height ? (ev.clientY - r.top) / r.height : 0;
    return { x: Math.max(0, Math.min(1, x)), y: Math.max(0, Math.min(1, y)) };
  }
  function mouseButtonName(ev) {
    if (ev.button === 2) return "right";
    if (ev.button === 1) return "middle";
    return "left";
  }
  function eventMods(ev) {
    var m = [];
    if (ev.metaKey) m.push("Meta");
    if (ev.ctrlKey) m.push("Control");
    if (ev.altKey) m.push("Alt");
    if (ev.shiftKey) m.push("Shift");
    return m;
  }
  function pointerPayload(el, ev) {
    var p = normEvent(el, ev);
    return { x: p.x, y: p.y, button: mouseButtonName(ev), clicks: ev.detail || 1, modifiers: eventMods(ev) };
  }
  function applyClipboardText(text) {
    if (!text || !navigator.clipboard || !navigator.clipboard.writeText) return;
    navigator.clipboard.writeText(text).catch(function () {});
  }
  function bindControl(el) {
    if (!el) return;
    el.addEventListener("contextmenu", function (ev) {
      ev.preventDefault();
      sendControl("click", pointerPayload(el, ev));
    });
    el.addEventListener("mousedown", function (ev) {
      ev.preventDefault();
      try { if (el.setPointerCapture && ev.pointerId != null) el.setPointerCapture(ev.pointerId); } catch (e) {}
      sendControl("down", pointerPayload(el, ev));
    });
    el.addEventListener("mouseup", function (ev) {
      sendControl("up", pointerPayload(el, ev));
    });
    el.addEventListener("mousemove", function (ev) {
      if (!ev.buttons) return;
      sendControl("move", pointerPayload(el, ev));
    });
    el.addEventListener("wheel", function (ev) {
      ev.preventDefault();
      var p = normEvent(el, ev);
      sendControl("scroll", { x: p.x, y: p.y, dy: ev.deltaY, dx: ev.deltaX, modifiers: eventMods(ev) });
    }, { passive: false });
  }
  function sendKeyEvent(ev, down) {
    if (!controlOn) return;
    if (ev.target && (ev.target.tagName === "INPUT" || ev.target.tagName === "TEXTAREA")) return;
    var mods = eventMods(ev);
    var accel = ev.metaKey || ev.ctrlKey || ev.altKey;
    if (down) ev.preventDefault();
    if (!accel && ev.key.length === 1 && down) {
      sendControl("type", { text: ev.key, modifiers: mods });
      return;
    }
    sendControl(down ? "keydown" : "keyup", { key: ev.key, down: down, modifiers: mods });
  }
  document.addEventListener("keydown", function (ev) { sendKeyEvent(ev, true); });
  document.addEventListener("keyup", function (ev) { sendKeyEvent(ev, false); });
  document.addEventListener("paste", function (ev) {
    if (!controlOn) return;
    if (ev.target && (ev.target.tagName === "INPUT" || ev.target.tagName === "TEXTAREA")) return;
    var text = ev.clipboardData && ev.clipboardData.getData("text/plain");
    if (!text) return;
    ev.preventDefault();
    sendControl("paste", { text: text });
  });
  bindControl(video);
  bindControl(canvas);
  var FILE_MAX = 64 * 1024 * 1024;
  var FILE_CHUNK = 24 * 1024;
  var incomingFiles = {};
  function sendFileJson(msg) {
    if (!controlOn || !ws || ws.readyState !== 1) return false;
    try { ws.send(JSON.stringify(msg)); return true; } catch (e) { return false; }
  }
  function bytesToB64(u8) {
    var s = "", step = 0x8000;
    for (var i = 0; i < u8.length; i += step) {
      s += String.fromCharCode.apply(null, u8.subarray(i, Math.min(i + step, u8.length)));
    }
    return btoa(s);
  }
  function b64ToBytes(b64) {
    var bin = atob(b64);
    var out = new Uint8Array(bin.length);
    for (var i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  }
  function triggerDownload(name, u8) {
    var a = document.createElement("a");
    a.href = URL.createObjectURL(new Blob([u8]));
    a.download = name || "file";
    a.click();
    setTimeout(function () { try { URL.revokeObjectURL(a.href); } catch (e) {} }, 2000);
  }
  function renderFileList(files) {
    var ul = document.getElementById("file-list");
    if (!ul) return;
    ul.innerHTML = "";
    (files || []).forEach(function (f) {
      var li = document.createElement("li");
      li.appendChild(document.createTextNode(f.name + (f.size != null ? " (" + f.size + " B)" : "")));
      var btn = document.createElement("button");
      btn.type = "button";
      btn.textContent = "Get";
      btn.addEventListener("click", function () {
        sendFileJson({ type: "file", action: "get", name: f.name });
      });
      li.appendChild(btn);
      ul.appendChild(li);
    });
  }
  function handleFileMsg(msg) {
    var out = document.getElementById("file-out");
    if (msg.action === "list") { renderFileList(msg.files || []); return; }
    if (msg.action === "ok") {
      if (out) out.textContent = "saved " + (msg.name || "");
      sendFileJson({ type: "file", action: "list" });
      return;
    }
    if (msg.action === "error" && out) { out.textContent = "error: " + (msg.error || "file"); return; }
    if (msg.action === "blob" && msg.data) {
      triggerDownload(msg.name, b64ToBytes(msg.data));
      return;
    }
    if (msg.action === "blob-begin") { incomingFiles[msg.name] = []; return; }
    if (msg.action === "blob-chunk" && incomingFiles[msg.name]) incomingFiles[msg.name].push(msg.data);
    if (msg.action === "blob-end" && incomingFiles[msg.name]) {
      var parts = incomingFiles[msg.name].map(b64ToBytes);
      var total = 0;
      parts.forEach(function (p) { total += p.length; });
      var u8 = new Uint8Array(total), off = 0;
      parts.forEach(function (p) { u8.set(p, off); off += p.length; });
      delete incomingFiles[msg.name];
      triggerDownload(msg.name, u8);
    }
  }
  function uploadFileBytes(name, u8) {
    var out = document.getElementById("file-out");
    if (u8.length > FILE_MAX) { if (out) out.textContent = "file too large (64 MB max)"; return; }
    if (out) out.textContent = "sending " + name + "…";
    var id = "f" + Date.now().toString(36);
    if (!sendFileJson({ type: "file", action: "begin", id: id, name: name, size: u8.length })) return;
    var off = 0;
    function pump() {
      var n = 0;
      while (n < 8 && off < u8.length) {
        var end = Math.min(off + FILE_CHUNK, u8.length);
        sendFileJson({ type: "file", action: "chunk", id: id, data: bytesToB64(u8.subarray(off, end)) });
        off = end;
        n += 1;
      }
      if (out) out.textContent = "sending " + name + " " + Math.min(100, Math.round((off * 100) / u8.length)) + "%";
      if (off >= u8.length) {
        sendFileJson({ type: "file", action: "end", id: id });
        return;
      }
      setTimeout(pump, 0);
    }
    pump();
  }
  function uploadDroppedFiles(fileList) {
    if (!controlOn) return;
    Array.prototype.forEach.call(fileList || [], function (file) {
      if (!file) return;
      file.arrayBuffer().then(function (buf) { uploadFileBytes(file.name, new Uint8Array(buf)); }).catch(function () {});
    });
  }
  function bindFileDrop(el) {
    if (!el) return;
    el.addEventListener("dragover", function (ev) {
      if (!controlOn) return;
      ev.preventDefault();
      el.classList.add("drop-hover");
    });
    el.addEventListener("dragleave", function () { el.classList.remove("drop-hover"); });
    el.addEventListener("drop", function (ev) {
      el.classList.remove("drop-hover");
      if (!controlOn) return;
      ev.preventDefault();
      uploadDroppedFiles(ev.dataTransfer && ev.dataTransfer.files);
    });
  }
  bindFileDrop(document.getElementById("file-drop"));
  bindFileDrop(video);
  var fileInput = document.getElementById("file-input");
  if (fileInput) {
    fileInput.addEventListener("change", function () {
      uploadDroppedFiles(fileInput.files);
      fileInput.value = "";
    });
  }
  function enqueue(kind, chunk) {
    if (kind === TYPE_INIT) {
      pending = [{ kind: kind, chunk: chunk }];
      return;
    }
    if (pending.length >= CAP) {
      var drop = -1;
      for (var i = 0; i < pending.length; i++) {
        if (pending[i].kind !== TYPE_INIT) { drop = i; break; }
      }
      if (drop >= 0) pending.splice(drop, 1);
      else pending.shift();
    }
    pending.push({ kind: kind, chunk: chunk });
  }
  function showTap() {
    tap.style.display = "flex";
  }
  tap.addEventListener("click", function () {
    tap.style.display = "none";
    video.play().catch(function () {});
  });

  function drawJpeg(payload) {
    if (!jpegMode) {
      jpegMode = true;
      video.style.display = "none";
      canvas.style.display = "block";
      pill.textContent = "live (jpeg)";
    }
    var blob = new Blob([payload], { type: "image/jpeg" });
    if (typeof createImageBitmap === "function") {
      createImageBitmap(blob).then(function (bmp) {
        if (canvas.width !== bmp.width || canvas.height !== bmp.height) {
          canvas.width = bmp.width; canvas.height = bmp.height;
        }
        var ctx = canvas.getContext("2d");
        ctx.drawImage(bmp, 0, 0);
        if (bmp.close) bmp.close();
      }).catch(function () {});
    } else {
      var img = new Image();
      img.onload = function () {
        canvas.width = img.width; canvas.height = img.height;
        canvas.getContext("2d").drawImage(img, 0, 0);
        URL.revokeObjectURL(img.src);
      };
      img.src = URL.createObjectURL(blob);
    }
  }

  function pump() {
    if (!sb || sb.updating) return;
    if (!liveSeeked && sb.buffered.length) {
      var start = sb.buffered.start(0);
      var end = sb.buffered.end(sb.buffered.length - 1);
      if (end - start >= 0.2) {
        liveSeeked = true;
        try { video.currentTime = Math.max(start, end - LIVE); } catch (e) {}
        video.play().catch(function () { showTap(); });
      }
    }
    if (sb.buffered.length && video.currentTime) {
      var lead = sb.buffered.end(sb.buffered.length - 1) - video.currentTime;
      try { video.playbackRate = lead > 1 ? 1.08 : 1; } catch (e) {}
    }
    if (!pending.length) return;
    var item = pending.shift();
    if (item.kind === TYPE_INIT) {
      try {
        if (sb.buffered.length) sb.remove(0, sb.buffered.end(sb.buffered.length - 1));
      } catch (e) {}
      liveSeeked = false;
      try { video.currentTime = 0; } catch (e) {}
      if (sb.buffered.length) { pending.unshift(item); return; }
    }
    try {
      sb.appendBuffer(item.chunk);
    } catch (e) {
      if (item.kind === TYPE_INIT) pending.unshift(item);
      startMse();
    }
  }

  function applyFlags(ctl) {
    controlOn = !!(ctl && (ctl.enabled || ctl.control));
    aiOn = !!(ctl && (ctl.ai_enabled || ctl.ai));
    var cu = document.getElementById("cu-section");
    if (cu) cu.style.display = aiOn ? "block" : "none";
    var files = document.getElementById("files-section");
    if (files) files.style.display = controlOn ? "block" : "none";
    if (controlOn) sendFileJson({ type: "file", action: "list" });
    renderDisplays(ctl && ctl.displays, ctl && (ctl.display || ctl.input));
  }
  function renderDisplays(list, current) {
    var wrap = document.getElementById("displays");
    if (!wrap) return;
    var devices = list || [];
    wrap.innerHTML = "";
    if (devices.length < 2) { wrap.style.display = "none"; return; }
    wrap.style.display = "flex";
    devices.forEach(function (d, i) {
      var b = document.createElement("button");
      b.type = "button";
      b.textContent = d.name ? d.name.replace(/ — .*/, "") : ("Display " + (i + 1));
      if (d.id === current || (!current && d.main)) b.className = "on";
      b.addEventListener("click", function () {
        if (!controlOn || !ws || ws.readyState !== 1) return;
        try { ws.send(JSON.stringify({ type: "display", id: d.id })); } catch (e) {}
      });
      wrap.appendChild(b);
    });
  }
  function handleText(text) {
    try {
      var msg = JSON.parse(text);
      if (msg && msg.type === "flags") {
        applyFlags({
          enabled: !!msg.control,
          ai_enabled: !!msg.ai,
          display: msg.display,
          displays: msg.displays,
        });
        return;
      }
      if (msg && msg.type === "analysis") {
        renderAnalysis(msg.data);
        refreshLlm();
      }
      if (msg && msg.type === "clipboard" && typeof msg.text === "string") {
        applyClipboardText(msg.text);
      }
      if (msg && msg.type === "file") handleFileMsg(msg);
    } catch (e) {}
  }

  function onBinary(type, payload) {
    if (type === TYPE_SNAP) return;
    if (type === TYPE_JPEG) { drawJpeg(payload); return; }
    if (type === TYPE_INIT || type === TYPE_FRAG) {
      enqueue(type, payload);
      pump();
    }
  }

  var onBinHandler = onBinary;
  function openWs(onBin) {
    if (onBin) onBinHandler = onBin;
    if (ws) {
      try { ws.onclose = null; } catch (e) {}
      try { if (ws._ping) clearInterval(ws._ping); } catch (e) {}
      try { ws.close(); } catch (e) {}
    }
    ws = new WebSocket(wsUrl());
    ws.binaryType = "arraybuffer";
    ws.onopen = function () {
      pill.textContent = jpegMode ? "live (jpeg)" : "live";
      if (ws._ping) clearInterval(ws._ping);
      ws._ping = setInterval(function () {
        if (ws && ws.readyState === 1) { try { ws.send("ping"); } catch (e) {} }
      }, 15000);
    };
    ws.onclose = function () {
      pill.textContent = "reconnecting";
      if (ws && ws._ping) { try { clearInterval(ws._ping); } catch (e) {} }
      if (reconnectTimer) return;
      reconnectTimer = setTimeout(function () {
        reconnectTimer = null;
        openWs(onBinHandler);
      }, 1000);
    };
    ws.onmessage = function (ev) {
      if (typeof ev.data === "string") { handleText(ev.data); return; }
      var buf = new Uint8Array(ev.data);
      if (!buf.length) return;
      onBinHandler(buf[0], buf.slice(1));
    };
  }

  function startJpeg() {
    jpegMode = true;
    video.style.display = "none";
    canvas.style.display = "block";
    openWs(function (type, payload) {
      if (type === TYPE_JPEG) drawJpeg(payload);
    });
  }

  function startMse() {
    if (!MS) {
      err.textContent = "This iPhone/iPad needs Safari 17.1+ (Managed Media Source) to play live H.264. Chrome on Android works. Or use the LAN page with encoder set to MJPEG.";
      pill.textContent = "unsupported";
      startJpeg();
      return;
    }
    if (ws) { try { ws.close(); } catch (e) {} }
    pending = []; liveSeeked = false;
    ms = new MS();
    video.style.display = "block";
    canvas.style.display = "none";
    video.src = URL.createObjectURL(ms);
    video.play().catch(function () { showTap(); });
    function onOpen() {
      var types = [
        'video/mp4; codecs="avc1.64001F"',
        'video/mp4; codecs="avc1.640028"',
        'video/mp4; codecs="avc1.4D401F"',
        'video/mp4; codecs="avc1.42E01E"',
        "video/mp4"
      ];
      var type = types.find(function (t) { return isTypeSupported(MS, t); }) || (isTypeSupported(MS, "video/mp4") ? "video/mp4" : null);
      if (!type) {
        err.textContent = "H.264 not playable here. Try Safari 17.1+ or Chrome on Android.";
        startJpeg();
        return;
      }
      try {
        sb = ms.addSourceBuffer(type);
        try { sb.mode = "sequence"; } catch (e) {}
      } catch (e) {
        err.textContent = "SourceBuffer: " + e.message;
        startJpeg();
        return;
      }
      sb.addEventListener("updateend", pump);
      openWs(onBinary);
    }
    ms.addEventListener("sourceopen", onOpen);
    ms.addEventListener("sourceended", function () {});
  }

  function apiPath(p) {
    var u = new URL(p, location.href);
    if (session) u.searchParams.set("session", session);
    return u.toString();
  }
  function fmtTs(ts) {
    return String(ts || "").replace("T", " ").slice(0, 19);
  }
  function renderAnalysis(a) {
    if (!a) return;
    document.getElementById("summary").textContent = a.error
      ? ("error: " + a.error)
      : (a.summary || "No analysis yet.");
    var wrap = document.getElementById("qs");
    wrap.innerHTML = "";
    (a.questions || []).forEach(function (q) {
      var d = document.createElement("div");
      d.className = "qa";
      var qe = document.createElement("div"); qe.className = "q"; qe.textContent = q.question || "";
      var ae = document.createElement("div"); ae.className = "a"; ae.textContent = q.answer || "";
      var me = document.createElement("div"); me.className = "meta";
      me.textContent = (q.confidence != null ? q.confidence + "% · " : "") + (q.reasoning || "");
      d.append(qe, ae, me);
      wrap.appendChild(d);
    });
  }
  function renderHistory(items) {
    var hist = document.getElementById("history");
    hist.innerHTML = "";
    (items || []).slice(0, 12).forEach(function (a) {
      var li = document.createElement("li");
      li.textContent = fmtTs(a.ts) + " — " + (a.summary || a.error || "");
      hist.appendChild(li);
    });
  }
  function refreshLlm() {
    fetch(apiPath("/api/analysis")).then(function (r) { return r.json(); }).then(function (body) {
      var note = document.getElementById("llm-note");
      var st = body.llm || {};
      if (!st.configured) note.textContent = "Attach a DeepSeek key later: npx wrangler secret put DEEPSEEK_API_KEY — then analysis and Ask start automatically.";
      else if (!st.has_snapshot) note.textContent = "Waiting for a screenshot from the origin (~8s)…";
      else note.textContent = "DeepSeek " + (st.model || "") + (st.analyzing ? " — analyzing" : " — ready");
      if (body.last) renderAnalysis(body.last);
      renderHistory(body.history);
      applyFlags(body.control || {});
    }).catch(function () {});
  }
  document.getElementById("ask-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var q = document.getElementById("ask-input").value.trim();
    if (!q) return;
    var out = document.getElementById("ask-out");
    out.textContent = "asking…";
    fetch(apiPath("/api/ask"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: q }),
    }).then(function (r) { return r.json(); }).then(function (body) {
      if (body.error) {
        out.textContent = "error: " + body.error;
        return;
      }
      out.textContent = (body.answer || "") + " (" + (body.confidence || 0) + "%)";
      if (body.reasoning) out.textContent += " — " + body.reasoning;
    }).catch(function (err) { out.textContent = String(err); });
  });
  document.getElementById("analyze").addEventListener("click", function () {
    document.getElementById("llm-note").textContent = "analyzing…";
    fetch(apiPath("/api/analyze-now"), { method: "POST" }).then(function (r) { return r.json(); }).then(function (body) {
      if (body.error && !body.summary) document.getElementById("llm-note").textContent = body.error;
      else renderAnalysis(body);
      refreshLlm();
    }).catch(function () {});
  });
  document.getElementById("cu-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var task = document.getElementById("cu-task").value.trim();
    if (!task) return;
    var out = document.getElementById("cu-out");
    out.textContent = "running…";
    fetch(apiPath("/api/computer-use"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ task: task }),
    }).then(function (r) { return r.json().then(function (b) { return { status: r.status, b: b }; }); }).then(function (x) {
      if (x.status === 403) out.textContent = "host has AI control off";
      else if (x.b.error) out.textContent = "error: " + x.b.error;
      else out.textContent = "accepted";
    }).catch(function (err) { out.textContent = String(err); });
  });

  var unlocked = false;
  function unlockPlayer(sess, expires) {
    if (unlocked) return;
    unlocked = true;
    if (sess) session = sess;
    if (expires) pill.title = "session ~" + Math.round(expires / 3600) + "h";
    document.getElementById("gate").style.display = "none";
    pill.textContent = "connecting…";
    startMse();
    refreshLlm();
    setInterval(refreshLlm, 4000);
  }

  document.getElementById("pin-form").addEventListener("submit", function (e) {
    e.preventDefault();
    var pin = (document.getElementById("pin").value || "").trim();
    err.textContent = "";
    fetch("/api/otp/redeem", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pin: pin }),
    }).then(function (r) { return r.json().then(function (b) { return { status: r.status, b: b }; }); }).then(function (x) {
      if (x.status !== 200 || !x.b.session) {
        err.textContent = x.b.error || "bad PIN";
        pill.textContent = "bad PIN";
        return;
      }
      unlockPlayer(x.b.session, x.b.expires_in_s);
    }).catch(function (ex) { err.textContent = String(ex); });
  });

  fetch("/api/analysis", { credentials: "include" }).then(function (r) {
    if (!r.ok) return;
    unlockPlayer("", 86400);
  }).catch(function () {});
})();
</script>
</body>
</html>
`;
