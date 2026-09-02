"use strict";

const LIVE_EDGE_S = 0.45;
const CATCHUP_LEAD_S = 1.0;
const CATCHUP_RATE = 1.08;
const PENDING_CAP = 24;
const TYPE_INIT = 1;
const TYPE_FRAG = 2;
const TYPE_JPEG = 3;
const TYPE_SNAP = 4;

function getCookie(name) {
  const esc = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = (typeof document !== "undefined" && document.cookie || "").match(
    new RegExp("(?:^|; )" + esc + "=([^;]*)")
  );
  return m ? decodeURIComponent(m[1]) : "";
}
function setCookie(name, value) {
  document.cookie = name + "=" + encodeURIComponent(value) + "; path=/; Max-Age=86400; SameSite=Lax";
}
function currentToken() {
  try {
    const q = new URLSearchParams(typeof location !== "undefined" ? location.search : "").get("token");
    if (q && q.trim()) return q.trim();
  } catch (e) { /* ignore */ }
  return getCookie("streamaid_token");
}
function hasViewerSession() {
  return getCookie("streamaid_viewer") === "1" || !!getCookie("streamaid_session");
}
function url(path) {
  let out = path;
  const token = currentToken();
  if (token && out.indexOf("token=") < 0) {
    out += (out.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(token);
  }
  const session = getCookie("streamaid_session");
  if (session && out.indexOf("session=") < 0) {
    out += (out.includes("?") ? "&" : "?") + "session=" + encodeURIComponent(session);
  }
  return out;
}

let cfg = null;
let mode = "ffmpeg";
let eventSource = null;
let ms = null, sb = null, ws = null, pending = [];
let mseReconnectTimer = null;
let liveSeeked = false;
let wsFails = 0;
let lastCT = -1, lastCTAt = 0, seekDeadline = 0;

function $(id) {
  return document.getElementById(id);
}

function isDrawerOpen() {
  const d = $("config-drawer");
  return !!(d && !d.classList.contains("hidden"));
}
function setHidden(el, hidden) {
  if (!el) return;
  el.classList.toggle("hidden", !!hidden);
  if ("hidden" in el) el.hidden = !!hidden;
}

function featureFlags() {
  const llmEl = $("cfg-llm-enabled");
  const aiEl = $("cfg-ai-enabled");
  const ctlEl = $("cfg-control-enabled");
  const audioEl = $("cfg-audio");
  const modeEl = $("cfg-mode");
  const mode = (modeEl && modeEl.value) || (cfg && cfg.encoder && cfg.encoder.mode) || "ffmpeg";
  return {
    llm: llmEl ? !!llmEl.checked : !!(cfg && cfg.llm && cfg.llm.enabled),
    ai: aiEl ? !!aiEl.checked : !!(cfg && cfg.control && cfg.control.ai_enabled),
    ctl: ctlEl ? !!ctlEl.checked : !!(cfg && cfg.control && cfg.control.enabled),
    audio: audioEl ? !!audioEl.checked : !!(cfg && cfg.capture && cfg.capture.audio),
    mjpeg: mode === "mjpeg",
  };
}

function syncEncoderUi() {
  const modeEl = $("cfg-mode");
  const mode = (modeEl && modeEl.value) || (cfg && cfg.encoder && cfg.encoder.mode) || "ffmpeg";
  const mjpeg = mode === "mjpeg";
  setHidden($("jpeg-fields"), !mjpeg);
  setHidden($("bitrate-fields"), mjpeg);
  setHidden($("gop-fields"), mjpeg);
}

function syncFeatureUi() {
  const f = featureFlags();
  setHidden($("llm-fields"), !f.llm);
  setHidden($("ctl-hint"), !f.ctl);
  setHidden($("ai-hint"), !f.ai);
  setHidden($("cu-cancel"), !f.ai);
  setHidden($("cu-section"), !f.ai);
  setHidden($("files-section"), !f.ctl);
  setHidden($("analysis-section"), !f.llm);
  setHidden($("analysis-banner"), !f.llm);
  setHidden($("analysis-pane"), !(f.llm || f.ai || f.ctl));
  setHidden($("audio-hint"), !f.audio);
  setHidden($("unmute"), !(f.audio && !f.mjpeg));
  syncEncoderUi();
  if (f.ctl) refreshFiles();
}

function setDrawerOpen(open) {
  const d = $("config-drawer");
  const backdrop = $("drawer-backdrop");
  if (d) d.classList.toggle("hidden", !open);
  if (backdrop) backdrop.classList.toggle("hidden", !open);
  if (open && d && cfg) fillConfigForm(cfg);
}
function closeDrawer() {
  setDrawerOpen(false);
}
function openDrawer() {
  setDrawerOpen(true);
}

async function api(path, opts) {
  opts = opts || {};
  const headers = Object.assign({}, opts.headers || {});
  const token = currentToken();
  if (token && !headers.Authorization) headers.Authorization = "Bearer " + token;
  const res = await fetch(url(path), Object.assign({}, opts, {
    headers: headers,
    credentials: "same-origin",
  }));
  if (res.status === 401) {
    if (!hasViewerSession()) showLogin();
    throw new Error("unauthorized");
  }
  return res;
}

function showLoginError(msg) {
  const el = $("login-error");
  if (el) el.textContent = msg || "";
}
function showLogin() {
  const el = $("login-overlay");
  if (el) el.classList.remove("hidden");
}
function hideLogin() {
  const el = $("login-overlay");
  if (el) el.classList.add("hidden");
  showLoginError("");
}

function fmtTs(ts) {
  return String(ts || "").replace("T", " ").slice(0, 19);
}

function renderStatus(s) {
  const cap = (s && s.capture) || {};
  const st = (s && s.stream) || {};
  const llm = (s && s.llm) || {};
  const q = (s && s.quality) || {};
  const fps = (cap.fps_actual || 0).toFixed(1);
  const label = st.mode === "ffmpeg" ? "h264" : (st.mode === "hevc" ? "hevc" : "mjpeg");
  const n = st.clients || 0;
  const el = $("status-pill");
  if (!el) return;
  el.textContent = fps + " fps · " + label + " · ws · " + n + " client" + (n === 1 ? "" : "s");
  el.classList.toggle("error", !!(cap.error || llm.last_error));
  el.title = (cap.error ? "capture: " + cap.error + " " : "") + (llm.last_error ? "llm: " + llm.last_error : "");
  const qp = $("quality-pill");
  if (!qp) return;
  if (q.last_check_at) {
    qp.textContent = "quality " + (q.score || 0) + "% " + (q.ok ? "✓" : "⚠");
    qp.classList.toggle("error", !q.ok);
  } else {
    qp.textContent = "quality —";
  }
}

function wsUrl() {
  // Same-origin UI always plays the local hub unless ?watch= is set.
  // cloudflare.watch_url is for the edge player, not this page — using it
  // here made localhost go black when the Worker publisher dropped.
  const q = new URLSearchParams(typeof location !== "undefined" ? location.search : "");
  const fromQuery = q.get("watch");
  const base = fromQuery || url("/stream.ws");
  const u = new URL(base, typeof location !== "undefined" ? location.href : "http://127.0.0.1/");
  u.protocol = u.protocol.replace("http", "ws");
  return u.toString();
}

function normEvent(el, ev) {
  const r = el.getBoundingClientRect();
  const x = r.width ? (ev.clientX - r.left) / r.width : 0;
  const y = r.height ? (ev.clientY - r.top) / r.height : 0;
  return { x: Math.max(0, Math.min(1, x)), y: Math.max(0, Math.min(1, y)) };
}

function sendControl(action, extra) {
  if (!ws || ws.readyState !== 1) return;
  const msg = Object.assign({ type: "control", action: action }, extra || {});
  try { ws.send(JSON.stringify(msg)); } catch (e) { /* ignore */ }
}

function mouseButtonName(ev) {
  if (ev.button === 2) return "right";
  if (ev.button === 1) return "middle";
  return "left";
}

function eventMods(ev) {
  const m = [];
  if (ev.metaKey) m.push("Meta");
  if (ev.ctrlKey) m.push("Control");
  if (ev.altKey) m.push("Alt");
  if (ev.shiftKey) m.push("Shift");
  return m;
}

function pointerPayload(el, ev) {
  const p = normEvent(el, ev);
  return {
    x: p.x,
    y: p.y,
    button: mouseButtonName(ev),
    clicks: ev.detail || 1,
    modifiers: eventMods(ev),
  };
}

function applyClipboardText(text) {
  if (!text || typeof navigator === "undefined" || !navigator.clipboard || !navigator.clipboard.writeText) return;
  navigator.clipboard.writeText(text).catch(function () {});
}

function applyClipboardPngBlob(blob) {
  if (!blob || typeof navigator === "undefined" || !navigator.clipboard || !window.ClipboardItem) return;
  navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]).catch(function () {});
}

function applyClipboardPng(b64) {
  if (!b64) return;
  try {
    const bin = atob(b64);
    const u8 = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
    applyClipboardPngBlob(new Blob([u8], { type: "image/png" }));
  } catch (e) { /* ignore */ }
}

const CLIP_PNG_MAX = 128 * 1024 * 1024;
const CLIP_PNG_CHUNK = 24 * 1024;
let incomingPng = null;

function sendClipboardPng(u8) {
  if (!u8 || !u8.length || u8.length > CLIP_PNG_MAX) return;
  if (u8.length <= CLIP_PNG_CHUNK) {
    sendControl("clipboard", { mime: "image/png", data: bytesToB64(u8) });
    return;
  }
  sendControl("clipboard", { mime: "image/png", phase: "begin", size: u8.length });
  let off = 0;
  function pump() {
    let n = 0;
    while (n < 8 && off < u8.length) {
      const end = Math.min(off + CLIP_PNG_CHUNK, u8.length);
      sendControl("clipboard", { mime: "image/png", phase: "chunk", data: bytesToB64(u8.subarray(off, end)) });
      off = end;
      n += 1;
    }
    if (off >= u8.length) sendControl("clipboard", { mime: "image/png", phase: "end" });
    else setTimeout(pump, 0);
  }
  pump();
}

function handleClipboardMsg(msg) {
  if (msg.mime === "image/png") {
    if (msg.action === "begin") {
      const size = Number(msg.size) || 0;
      incomingPng = (size > 0 && size <= CLIP_PNG_MAX) ? { size: size, parts: [], got: 0 } : null;
      return;
    }
    if (msg.action === "chunk" && incomingPng) {
      const u8 = b64ToBytes(msg.data);
      if (!u8 || incomingPng.got + u8.length > incomingPng.size || incomingPng.got + u8.length > CLIP_PNG_MAX) {
        incomingPng = null;
        return;
      }
      incomingPng.parts.push(u8);
      incomingPng.got += u8.length;
      return;
    }
    if (msg.action === "end" && incomingPng) {
      const done = incomingPng;
      incomingPng = null;
      if (done.got !== done.size) return;
      applyClipboardPngBlob(new Blob(done.parts, { type: "image/png" }));
      return;
    }
    if (msg.data) applyClipboardPng(msg.data);
    return;
  }
  if (typeof msg.text === "string") applyClipboardText(msg.text);
}

function pasteImageFile(file) {
  if (!file || file.size > CLIP_PNG_MAX) return;
  function fromBuf(buf) { sendClipboardPng(new Uint8Array(buf)); }
  if (file.type === "image/png") {
    file.arrayBuffer().then(fromBuf).catch(function () {});
    return;
  }
  if (typeof createImageBitmap !== "function") {
    file.arrayBuffer().then(fromBuf).catch(function () {});
    return;
  }
  createImageBitmap(file).then(function (bmp) {
    const c = document.createElement("canvas");
    c.width = bmp.width;
    c.height = bmp.height;
    const ctx = c.getContext("2d");
    if (!ctx) return;
    ctx.drawImage(bmp, 0, 0);
    c.toBlob(function (blob) {
      if (!blob) return;
      blob.arrayBuffer().then(fromBuf).catch(function () {});
    }, "image/png");
  }).catch(function () {
    file.arrayBuffer().then(fromBuf).catch(function () {});
  });
}

const FILE_MAX = 2 * 1024 * 1024 * 1024;
const FILE_CHUNK = 24 * 1024;
const pendingUploads = {};

function bytesToB64(u8) {
  let s = "";
  const step = 0x8000;
  for (let i = 0; i < u8.length; i += step) {
    s += String.fromCharCode.apply(null, u8.subarray(i, Math.min(i + step, u8.length)));
  }
  return btoa(s);
}

function sendFileJson(msg) {
  if (ws && ws.readyState === 1) {
    try { ws.send(JSON.stringify(msg)); return true; } catch (e) { /* fall through */ }
  }
  return false;
}

function renderFileList(files) {
  const ul = $("file-list");
  if (!ul) return;
  ul.innerHTML = "";
  (files || []).forEach(function (f) {
    const li = document.createElement("li");
    const a = document.createElement("a");
    a.href = url("/api/files/download?name=" + encodeURIComponent(f.name));
    a.textContent = f.name + (f.size != null ? " (" + f.size + " B)" : "");
    a.download = f.name;
    li.appendChild(a);
    ul.appendChild(li);
  });
}

function refreshFiles() {
  if (!featureFlags().ctl) return;
  sendFileJson({ type: "file", action: "list" });
  if (typeof fetch !== "function") return;
  fetch(url("/api/files")).then(function (r) { return r.json(); }).then(function (body) {
    if (body && body.files) renderFileList(body.files);
  }).catch(function () {});
}

function uploadFile(file) {
  const out = $("file-out");
  if (!file) return;
  if (file.size > FILE_MAX) {
    if (out) out.textContent = "file too large (2 GB max)";
    return;
  }
  if (ws && ws.readyState === 1) {
    if (out) out.textContent = "sending " + file.name + "…";
    const id = "f" + Date.now().toString(36) + Math.random().toString(36).slice(2, 6);
    pendingUploads[id] = { name: file.name, file: file };
    sendFileJson({ type: "file", action: "begin", id: id, name: file.name, size: file.size });
    return;
  }
  if (file.size > 8 * 1024 * 1024) {
    if (out) out.textContent = "file too large for HTTP; wait for the live session";
    return;
  }
  if (typeof fetch !== "function") return;
  file.arrayBuffer().then(function (buf) {
    const u8 = new Uint8Array(buf);
    return fetch(url("/api/files"), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ name: file.name, data: bytesToB64(u8) }),
    });
  }).then(function (r) { return r && r.json(); }).then(function (body) {
    if (!body) return;
    if (out) out.textContent = body.error ? ("error: " + body.error) : ("saved " + (body.name || file.name));
    refreshFiles();
  }).catch(function (err) {
    if (out) out.textContent = "error: " + err.message;
  });
}

function uploadDroppedFiles(fileList) {
  if (!featureFlags().ctl) return;
  Array.prototype.forEach.call(fileList || [], function (file) {
    uploadFile(file);
  });
}

function clipboardHasNonImageFiles(files) {
  if (!files || !files.length) return false;
  for (let i = 0; i < files.length; i++) {
    const t = files[i].type || "";
    if (t.indexOf("image/") !== 0) return true;
  }
  return false;
}

function bindControl(el) {
  if (!el || el.dataset.ctlBound) return;
  el.dataset.ctlBound = "1";
  el.addEventListener("contextmenu", function (ev) {
    ev.preventDefault();
    sendControl("click", pointerPayload(el, ev));
  });
  el.addEventListener("mousedown", function (ev) {
    ev.preventDefault();
    try { if (el.setPointerCapture && ev.pointerId != null) el.setPointerCapture(ev.pointerId); } catch (e) { /* ignore */ }
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
    const p = normEvent(el, ev);
    sendControl("scroll", { x: p.x, y: p.y, dy: ev.deltaY, dx: ev.deltaX, modifiers: eventMods(ev) });
  }, { passive: false });
}

function isIOS() {
  const ua = (typeof navigator !== "undefined" && navigator.userAgent) || "";
  return /iPad|iPhone|iPod/.test(ua);
}

function showStreamError(msg) {
  const el = $("stream-error");
  if (!el) return;
  el.textContent = msg;
  el.classList.remove("hidden");
}

function hideVideoLike() {
  const img = $("stream-img");
  const video = $("stream-video");
  const canvas = $("stream-canvas");
  if (video) { video.pause(); video.removeAttribute("src"); video.classList.add("hidden"); }
  if (img) img.classList.add("hidden");
  if (canvas) canvas.classList.add("hidden");
}

function enqueue(kind, chunk) {
  if (kind === TYPE_INIT) {
    pending = [{ kind: kind, chunk: chunk }];
    return;
  }
  if (pending.length >= PENDING_CAP) {
    const drop = pending.findIndex(function (p) { return p.kind !== TYPE_INIT; });
    if (drop >= 0) pending.splice(drop, 1);
    else pending.shift();
  }
  pending.push({ kind: kind, chunk: chunk });
}

function teardownMse() {
  if (mseReconnectTimer) { clearTimeout(mseReconnectTimer); mseReconnectTimer = null; }
  if (ws) { try { ws.close(); } catch (e) { /* ignore */ } ws = null; }
  if (sb) { try { sb.removeEventListener("updateend", pump); } catch (e) { /* ignore */ } }
  const video = $("stream-video");
  if (video) {
    video.removeEventListener("timeupdate", pruneBuffer);
    video.removeEventListener("error", onVideoError);
    if (video.src && video.src.indexOf("blob:") === 0) {
      try { URL.revokeObjectURL(video.src); } catch (e) { /* ignore */ }
    }
  }
  sb = null; ms = null; pending = []; liveSeeked = false;
}

function safeAppend(chunk) {
  try {
    sb.appendBuffer(chunk);
  } catch (e) {
    rebuildMse();
  }
}

function handleNewStream() {
  try {
    if (sb.buffered.length) sb.remove(0, sb.buffered.end(sb.buffered.length - 1));
  } catch (e) { /* ignore */ }
  liveSeeked = false;
  const video = $("stream-video");
  if (video) video.currentTime = 0;
}

function maybeLiveSeek() {
  const video = $("stream-video");
  if (!sb || !sb.buffered.length || !video) return;
  const start = sb.buffered.start(0);
  const end = sb.buffered.end(sb.buffered.length - 1);
  if (end - start < 0.05) return;
  // Capture PTS can be hours in; playing at t=0 is a black screen.
  if (!liveSeeked || video.currentTime < start - 0.25) {
    liveSeeked = true;
    video.currentTime = Math.max(start, end - LIVE_EDGE_S);
    seekDeadline = (typeof performance !== "undefined" ? performance.now() : 0) + 1500;
    video.play().catch(function () {});
  }
}

function catchUp() {
  const video = $("stream-video");
  if (!sb || !sb.buffered.length || !video) return;
  const end = sb.buffered.end(sb.buffered.length - 1);
  const lead = end - video.currentTime;
  if (lead > CATCHUP_LEAD_S) video.playbackRate = CATCHUP_RATE;
  else if (lead < 0.35) video.playbackRate = 1.0;
}

function watchPlayback() {
  const video = $("stream-video");
  if (!sb || !sb.buffered.length || !video) return;
  const now = typeof performance !== "undefined" ? performance.now() : 0;
  const ct = video.currentTime;
  const end = sb.buffered.end(sb.buffered.length - 1);
  if (video.seeking) {
    if (seekDeadline && now > seekDeadline) {
      video.currentTime = sb.buffered.start(0) + 0.05;
      seekDeadline = now + 1500;
    }
    return;
  }
  if (ct === lastCT) {
    if (now - lastCTAt > 2000 && end - ct > 0.8) {
      video.currentTime = Math.max(sb.buffered.start(0), end - LIVE_EDGE_S);
      seekDeadline = now + 1500;
    }
  } else {
    lastCT = ct;
    lastCTAt = now;
  }
  catchUp();
}

function pruneBuffer() {
  const video = $("stream-video");
  if (!sb || sb.updating || !video || !video.buffered.length) return;
  if (video.currentTime > 1) {
    try { sb.remove(0, video.currentTime - 1); } catch (e) { /* ignore */ }
  }
}

function pump() {
  if (!sb || sb.updating) return;
  maybeLiveSeek();
  watchPlayback();
  if (pending.length) {
    const item = pending.shift();
    if (item.kind === TYPE_INIT) {
      handleNewStream();
      if (sb.buffered.length) { pending.unshift(item); return; }
    }
    safeAppend(item.chunk);
  }
}

function onVideoError() {
  if (ms && $("stream-video") && $("stream-video").error) rebuildMse();
}

function rebuildMse() {
  showStreamError("stream error — reconnecting");
  startMse();
}

function scheduleMseReconnect() {
  if (mseReconnectTimer) return;
  const delay = Math.min(1000 * Math.pow(2, Math.min(wsFails, 3)), 8000);
  mseReconnectTimer = setTimeout(function () {
    mseReconnectTimer = null;
    startMse();
  }, delay);
}

function startChunkPump(id, name, file, off) {
  const out = $("file-out");
  const total = file.size;
  function pump() {
    if (off >= total) {
      sendFileJson({ type: "file", action: "end", id: id });
      if (out) out.textContent = "sending " + name + " 100%";
      return;
    }
    function next(n) {
      if (off >= total) {
        sendFileJson({ type: "file", action: "end", id: id });
        if (out) out.textContent = "sending " + name + " 100%";
        return;
      }
      if (n >= 8) {
        setTimeout(pump, 0);
        return;
      }
      const end = Math.min(off + FILE_CHUNK, total);
      file.slice(off, end).arrayBuffer().then(function (buf) {
        sendFileJson({ type: "file", action: "chunk", id: id, data: bytesToB64(new Uint8Array(buf)) });
        off = end;
        if (out) {
          out.textContent = "sending " + name + " " + Math.min(100, Math.round((off * 100) / total)) + "%";
        }
        next(n + 1);
      }).catch(function () {
        if (out) out.textContent = "error: failed to read " + name;
      });
    }
    next(0);
  }
  pump();
}

function handleFileMsg(msg) {
  const out = $("file-out");
  if (msg.action === "accept" && msg.id && pendingUploads[msg.id]) {
    const job = pendingUploads[msg.id];
    delete pendingUploads[msg.id];
    startChunkPump(msg.id, job.name, job.file, msg.offset || 0);
    return;
  }
  if (msg.action === "list" && msg.files) {
    renderFileList(msg.files);
    return;
  }
  if (msg.action === "ok") {
    if (out) out.textContent = "saved " + (msg.name || "");
    refreshFiles();
    return;
  }
  if (msg.action === "error" && out) out.textContent = "error: " + (msg.error || "file");
}

function onWsMessage(ev) {
  if (typeof ev.data === "string") {
    try {
      const msg = JSON.parse(ev.data);
      if (msg && msg.type === "clipboard") handleClipboardMsg(msg);
      if (msg && msg.type === "thumbs") applyDisplayThumbs(msg.items);
      if (msg && msg.type === "file") {
        handleFileMsg(msg);
      }
    } catch (e) { /* ignore */ }
    return;
  }
  const buf = new Uint8Array(ev.data);
  if (!buf.length) return;
  const type = buf[0];
  const payload = buf.slice(1);
  if (type === TYPE_SNAP) return;
  if (type === TYPE_JPEG) {
    drawJpeg(payload);
    return;
  }
  if (type === TYPE_INIT || type === TYPE_FRAG) {
    enqueue(type, payload);
    pump();
  }
}

function workerHttp(path) {
  const w = cfg && cfg.cloudflare && cfg.cloudflare.watch_url;
  if (!w) return null;
  try {
    const u = new URL(w);
    u.protocol = u.protocol === "wss:" ? "https:" : "http:";
    u.pathname = path;
    return u.toString();
  } catch (e) {
    return null;
  }
}

function renderEdgeAnalysis(body) {
  const note = $("llm-note");
  const st = (body && body.llm) || {};
  if (note) {
    if (!st.configured) {
      note.textContent = "Worker ready. Attach DeepSeek later: npx wrangler secret put DEEPSEEK_API_KEY";
    } else if (!st.has_snapshot) {
      note.textContent = "Waiting for a screenshot from this origin (~8s)…";
    } else {
      note.textContent = "DeepSeek " + (st.model || "") + (st.analyzing ? " — analyzing" : " — ready");
    }
  }
  const last = body && body.last;
  const sum = $("latest-summary");
  if (sum && last) {
    sum.textContent = last.error ? ("error: " + last.error) : (last.summary || "No analysis yet.");
  }
  const qs = $("questions");
  if (qs) {
    qs.innerHTML = "";
    ((last && last.questions) || []).forEach(function (q) {
      const d = document.createElement("div");
      d.className = "qa";
      const qe = document.createElement("div");
      qe.className = "q";
      qe.textContent = q.question || "";
      const ae = document.createElement("div");
      ae.className = "a";
      ae.textContent = (q.answer || "") + (q.confidence != null ? " (" + q.confidence + "%)" : "");
      d.append(qe, ae);
      qs.appendChild(d);
    });
  }
  const hist = $("history");
  if (hist) {
    hist.innerHTML = "";
    ((body && body.history) || []).slice(0, 12).forEach(function (a) {
      const li = document.createElement("li");
      li.textContent = fmtTs(a.ts) + " — " + (a.summary || a.error || "");
      hist.appendChild(li);
    });
  }
}

async function refreshEdgeAnalysis() {
  const href = workerHttp("/api/analysis");
  if (!href) return;
  try {
    const res = await fetch(href);
    const body = await res.json();
    renderEdgeAnalysis(body);
  } catch (e) { /* ignore */ }
}

function connectWsForMse() {
  if (typeof WebSocket === "undefined") {
    showStreamError("WebSocket unsupported");
    return;
  }
  try {
    ws = new WebSocket(wsUrl());
  } catch (e) {
    wsFails += 1;
    scheduleMseReconnect();
    return;
  }
  ws.binaryType = "arraybuffer";
  ws.onopen = function () { wsFails = 0; };
  ws.onmessage = onWsMessage;
  ws.onerror = function () { wsFails += 1; };
  ws.onclose = function () {
    ws = null;
    scheduleMseReconnect();
  };
}

function mediaSourceCtor() {
  return window.ManagedMediaSource || window.MediaSource || window.WebKitMediaSource || null;
}

function onSourceOpen() {
  const MS = mediaSourceCtor();
  const hevc = mode === "hevc";
  const audio = featureFlags().audio && !featureFlags().mjpeg;
  const candidates = hevc
    ? (audio
        ? ['video/mp4; codecs="hvc1.1.6.L93.B0,mp4a.40.2"', 'video/mp4; codecs="hev1.1.6.L93.B0,mp4a.40.2"', 'video/mp4; codecs="hvc1.1.6.L93.B0"', "video/mp4"]
        : ['video/mp4; codecs="hvc1.1.6.L93.B0"', 'video/mp4; codecs="hev1.1.6.L93.B0"', "video/mp4"])
    : (audio
        ? [
            'video/mp4; codecs="avc1.64001F,mp4a.40.2"',
            'video/mp4; codecs="avc1.640028,mp4a.40.2"',
            'video/mp4; codecs="avc1.64001F"',
            'video/mp4; codecs="avc1.640028"',
            "video/mp4",
          ]
        : [
            'video/mp4; codecs="avc1.64001F"',
            'video/mp4; codecs="avc1.640028"',
            'video/mp4; codecs="avc1.4D401F"',
            'video/mp4; codecs="avc1.42E01E"',
            "video/mp4",
          ]);
  const type = candidates.find(function (t) {
    try { return !!(MS && MS.isTypeSupported && MS.isTypeSupported(t)); } catch (e) { return false; }
  });
  if (!type) {
    showStreamError((hevc ? "HEVC" : "MP4") + " not supported — switch encoder");
    return;
  }
  try {
    sb = ms.addSourceBuffer(type);
    try { sb.mode = "sequence"; } catch (e) { /* ignore */ }
  } catch (e) {
    showStreamError("SourceBuffer error: " + e.message);
    return;
  }
  sb.addEventListener("updateend", pump);
  const video = $("stream-video");
  if (video) {
    video.addEventListener("timeupdate", pruneBuffer);
    video.play().catch(function () {});
  }
  connectWsForMse();
}

function startMse() {
  const MS = mediaSourceCtor();
  if (!MS) {
    showStreamError("This browser needs Safari 17.1+ (or Chrome on Android) for live H.264. Or set encoder to MJPEG.");
    return;
  }
  teardownMse();
  const video = $("stream-video");
  if (!video) return;
  video.classList.remove("hidden");
  video.setAttribute("playsinline", "");
  video.setAttribute("webkit-playsinline", "true");
  video.playsInline = true;
  video.muted = true;
  video.disableRemotePlayback = true;
  ms = new MS();
  video.src = URL.createObjectURL(ms);
  ms.addEventListener("sourceopen", onSourceOpen);
  video.addEventListener("error", onVideoError);
}

function drawJpeg(jpeg) {
  const canvas = $("stream-canvas");
  if (!canvas) return;
  canvas.classList.remove("hidden");
  const ctx = canvas.getContext("2d");
  if (!ctx || typeof createImageBitmap === "undefined") return;
  createImageBitmap(new Blob([jpeg], { type: "image/jpeg" })).then(function (bmp) {
    if (canvas.width !== bmp.width || canvas.height !== bmp.height) {
      canvas.width = bmp.width;
      canvas.height = bmp.height;
    }
    ctx.drawImage(bmp, 0, 0);
    if (bmp.close) bmp.close();
  }).catch(function () {});
}

function startJpegWs() {
  hideVideoLike();
  const canvas = $("stream-canvas");
  if (canvas) canvas.classList.remove("hidden");
  if (ws) { try { ws.close(); } catch (e) { /* ignore */ } }
  ws = new WebSocket(wsUrl());
  ws.binaryType = "arraybuffer";
  ws.onmessage = onWsMessage;
  ws.onclose = function () { setTimeout(startJpegWs, 1000); };
}

function showMjpeg() {
  teardownMse();
  hideVideoLike();
  if (typeof WebSocket !== "undefined") startJpegWs();
}

function showH264() {
  hideVideoLike();
  startMse();
}

function layoutDisplayMap(devices, boxW, boxH) {
  const list = devices || [];
  if (!list.length) return [];
  const hasGeo = list.some(function (d) { return (d.width || 0) > 0 && (d.height || 0) > 0; });
  if (!hasGeo) {
    return list.map(function (d, i) {
      const w = Math.max(8, boxW / list.length - 4);
      return { id: d.id, left: i * (w + 4), top: 2, width: w, height: boxH - 4, label: String(i + 1) };
    });
  }
  let minX = Infinity, minY = Infinity, maxX = -Infinity, maxY = -Infinity;
  list.forEach(function (d) {
    const x = d.x || 0, y = d.y || 0, w = d.width || 1, h = d.height || 1;
    minX = Math.min(minX, x);
    minY = Math.min(minY, y);
    maxX = Math.max(maxX, x + w);
    maxY = Math.max(maxY, y + h);
  });
  const spanX = Math.max(1, maxX - minX);
  const spanY = Math.max(1, maxY - minY);
  const scale = Math.min((boxW - 4) / spanX, (boxH - 4) / spanY);
  return list.map(function (d, i) {
    const x = d.x || 0, y = d.y || 0, w = d.width || 1, h = d.height || 1;
    return {
      id: d.id,
      left: 2 + (x - minX) * scale,
      top: 2 + (y - minY) * scale,
      width: Math.max(8, w * scale),
      height: Math.max(8, h * scale),
      label: String(i + 1),
    };
  });
}

function liveStreamSource() {
  const video = $("stream-video");
  if (video && !video.classList.contains("hidden") && video.videoWidth) return video;
  const canvas = $("stream-canvas");
  if (canvas && !canvas.classList.contains("hidden") && canvas.width) return canvas;
  return null;
}

function b64ToBytes(b64) {
  try {
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch (e) {
    return null;
  }
}

function applyDisplayThumbs(items) {
  const map = $("display-map");
  if (!map || map.classList.contains("hidden")) return;
  (items || []).forEach(function (it) {
    if (!it || !it.id || !it.data) return;
    const tiles = map.querySelectorAll(".mon");
    for (let i = 0; i < tiles.length; i++) {
      const b = tiles[i];
      if (b.dataset.id !== it.id || b.classList.contains("on")) continue;
      const c = b.querySelector("canvas.mon-thumb");
      if (!c) continue;
      const u8 = b64ToBytes(it.data);
      if (!u8 || typeof createImageBitmap === "undefined") continue;
      const seq = (Number(c.dataset.thumbSeq) || 0) + 1;
      c.dataset.thumbSeq = String(seq);
      createImageBitmap(new Blob([u8], { type: "image/jpeg" })).then(function (bmp) {
        if (c.dataset.thumbSeq !== String(seq) || b.classList.contains("on")) {
          if (bmp.close) bmp.close();
          return;
        }
        c.width = bmp.width;
        c.height = bmp.height;
        const ctx = c.getContext("2d");
        if (ctx) ctx.drawImage(bmp, 0, 0);
        if (bmp.close) bmp.close();
      }).catch(function () {});
    }
  });
}

function paintMapThumbs() {
  const map = $("display-map");
  if (!map || map.classList.contains("hidden")) return;
  const dest = map.querySelector(".mon.on canvas.mon-thumb");
  if (!dest) return;
  const src = liveStreamSource();
  if (!src) return;
  const w = src.videoWidth || src.width;
  const h = src.videoHeight || src.height;
  if (!w || !h) return;
  if (dest.width !== w || dest.height !== h) {
    dest.width = w;
    dest.height = h;
  }
  const ctx = dest.getContext("2d");
  if (!ctx) return;
  try { ctx.drawImage(src, 0, 0, w, h); } catch (e) { /* frame not ready */ }
}

function renderDisplayMap(el, devices, current, onPick) {
  if (!el) return;
  const list = devices || [];
  if (list.length < 2) {
    setHidden(el, true);
    el.innerHTML = "";
    delete el.dataset.idsKey;
    return;
  }
  setHidden(el, false);
  const idsKey = list.map(function (d) { return d.id; }).join("|");
  if (el.dataset.idsKey === idsKey && el.querySelector(".mon-thumb")) {
    el.querySelectorAll(".mon").forEach(function (b) {
      b.classList.toggle("on", b.dataset.id === current);
    });
    return;
  }
  const prev = {};
  el.querySelectorAll(".mon").forEach(function (b) {
    const c = b.querySelector("canvas.mon-thumb");
    if (c && b.dataset.id) prev[b.dataset.id] = c;
  });
  el.innerHTML = "";
  el.dataset.idsKey = idsKey;
  const boxW = el.clientWidth || 220;
  const boxH = el.clientHeight || 100;
  layoutDisplayMap(list, boxW, boxH).forEach(function (m) {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "mon" + (m.id === current ? " on" : "");
    b.dataset.id = m.id;
    b.style.left = m.left + "px";
    b.style.top = m.top + "px";
    b.style.width = m.width + "px";
    b.style.height = m.height + "px";
    b.title = m.id;
    const c = document.createElement("canvas");
    c.className = "mon-thumb";
    const old = prev[m.id];
    if (old && old.width) {
      c.width = old.width;
      c.height = old.height;
      try { c.getContext("2d").drawImage(old, 0, 0); } catch (e) { /* ignore */ }
    }
    b.appendChild(c);
    const lab = document.createElement("span");
    lab.className = "mon-label";
    lab.textContent = m.label;
    b.appendChild(lab);
    b.addEventListener("click", function () { if (onPick) onPick(m.id); });
    el.appendChild(b);
  });
}

function fillDisplaySelect(sel, devices, current) {
  if (!sel) return;
  const list = devices || [];
  sel.innerHTML = "";
  if (!list.length) {
    const opt = document.createElement("option");
    opt.value = "";
    opt.textContent = "auto-detect";
    sel.appendChild(opt);
  }
  list.forEach(function (d) {
    const opt = document.createElement("option");
    opt.value = d.id;
    opt.textContent = d.name || d.id;
    sel.appendChild(opt);
  });
  if (current) sel.value = current;
  setHidden(sel, list.length === 0 && sel.id === "display-pill");
}

function loadDisplays() {
  const current = (cfg && cfg.capture && cfg.capture.input) || ($("cfg-input") && $("cfg-input").value) || "";
  const apply = function (devices) {
    fillDisplaySelect($("cfg-display"), devices, current);
    fillDisplaySelect($("display-pill"), devices, current);
    renderDisplayMap($("display-map"), devices, current, function (id) {
      const input = $("cfg-input");
      if (input) input.value = id;
      const pill = $("display-pill");
      if (pill) pill.value = id;
      if (typeof fetch !== "function") return;
      api("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ capture: { input: id } }),
      }).catch(function () {});
    });
    const input = $("cfg-input");
    if (input && $("cfg-display") && $("cfg-display").value) input.value = $("cfg-display").value;
  };
  if (typeof fetch !== "function") return;
  fetch(url("/api/capture-devices")).then(function (r) { return r.json(); }).then(apply).catch(function () {
    fetch(url("/api/status")).then(function (r) { return r.json(); }).then(function (s) {
      apply((s && s.capture && s.capture.displays) || []);
    }).catch(function () {});
  });
}

function fillConfigForm(c) {
  if (!c) return;
  function set(id, val) { const el = $(id); if (el) el.value = val; }
  set("cfg-host", c.host);
  set("cfg-port", c.port);
  set("cfg-token", c.token);
  set("cfg-input", c.capture && c.capture.input || "");
  loadDisplays();
  set("cfg-fps", c.capture && c.capture.fps);
  set("cfg-scale", String(c.capture && c.capture.scale));
  set("cfg-jpeg", c.capture && c.capture.jpeg_quality);
  const jv = $("jpeg-val");
  if (jv) jv.textContent = c.capture && c.capture.jpeg_quality;
  set("cfg-mode", c.encoder && c.encoder.mode);
  syncEncoderUi();
  set("cfg-bitrate", c.encoder && c.encoder.bitrate_kbps);
  set("cfg-gop", c.encoder && c.encoder.gop_frames || 15);
  set("cfg-max-w", c.encoder && c.encoder.max_width || 3840);
  set("cfg-max-h", c.encoder && c.encoder.max_height || 4320);
  set("cfg-publish", c.cloudflare && c.cloudflare.publish_url || "");
  set("cfg-watch", c.cloudflare && c.cloudflare.watch_url || "");
  const ctl = $("cfg-control-enabled");
  if (ctl) ctl.checked = !!(c.control && c.control.enabled);
  const ai = $("cfg-ai-enabled");
  if (ai) ai.checked = !!(c.control && c.control.ai_enabled);
  const en = $("cfg-llm-enabled");
  if (en) en.checked = !!(c.llm && c.llm.enabled);
  const aud = $("cfg-audio");
  if (aud) aud.checked = !!(c.capture && c.capture.audio);
  set("cfg-base-url", c.llm && c.llm.base_url || "");
  set("cfg-api-key", c.llm && c.llm.api_key || "");
  set("cfg-model", c.llm && c.llm.model || "");
  set("cfg-interval", c.llm && c.llm.interval_sec);
  set("cfg-prompt", c.llm && c.llm.prompt || "");
  syncFeatureUi();
}

function readConfigForm() {
  function val(id) { const el = $(id); return el ? el.value : ""; }
  const llmEn = $("cfg-llm-enabled");
  const ctlEn = $("cfg-control-enabled");
  const aiEn = $("cfg-ai-enabled");
  return {
    host: val("cfg-host"),
    port: parseInt(val("cfg-port"), 10) || 8080,
    token: val("cfg-token"),
    control: {
      enabled: !!(ctlEn && ctlEn.checked),
      ai_enabled: !!(aiEn && aiEn.checked),
    },
    capture: {
      driver: "ffmpeg",
      input: val("cfg-input"),
      fps: parseInt(val("cfg-fps"), 10) || 30,
      scale: parseFloat(val("cfg-scale")) || 1.0,
      jpeg_quality: parseInt(val("cfg-jpeg"), 10) || 95,
      audio: !!( $("cfg-audio") && $("cfg-audio").checked ),
    },
    encoder: {
      mode: val("cfg-mode") || "ffmpeg",
      bitrate_kbps: parseInt(val("cfg-bitrate"), 10) || 20000,
      gop_frames: parseInt(val("cfg-gop"), 10) || 15,
      max_width: parseInt(val("cfg-max-w"), 10) || 3840,
      max_height: parseInt(val("cfg-max-h"), 10) || 4320,
    },
    cloudflare: {
      publish_url: val("cfg-publish"),
      watch_url: val("cfg-watch"),
    },
    llm: {
      enabled: !!(llmEn && llmEn.checked),
      base_url: val("cfg-base-url"),
      api_key: val("cfg-api-key"),
      model: val("cfg-model"),
      interval_sec: parseInt(val("cfg-interval"), 10) || 5,
      prompt: val("cfg-prompt"),
    },
  };
}

async function loadConfig() {
  try {
    const res = await fetch(url("/api/config"));
    if (res.status === 401) {
      if (hasViewerSession()) {
        hideLogin();
        showH264();
        return;
      }
      showLogin();
      return;
    }
    cfg = await res.json();
  } catch (e) {
    if (hasViewerSession()) showH264();
    return;
  }
  hideLogin();
  mode = (cfg.encoder && cfg.encoder.mode) || "ffmpeg";
  fillConfigForm(cfg);
  syncFeatureUi();
  if (mode === "mjpeg") showMjpeg();
  else showH264();
}

function connectEvents() {
  if (typeof EventSource === "undefined") return;
  if (eventSource) eventSource.close();
  eventSource = new EventSource(url("/api/events"));
  eventSource.addEventListener("status", function (e) {
    try { renderStatus(JSON.parse(e.data)); } catch (err) { /* ignore */ }
  });
  eventSource.addEventListener("config-applied", function () { loadConfig(); });
  eventSource.onerror = function () {
    eventSource.close();
    eventSource = null;
  };
}

function onReady() {
  const bootToken = currentToken();
  if (bootToken && !getCookie("streamaid_token")) setCookie("streamaid_token", bootToken);
  const login = $("login-form");
  if (login) {
    login.addEventListener("submit", function (e) {
      e.preventDefault();
      const pin = ($("login-pin") && $("login-pin").value || "").trim();
      const t = ($("login-token") && $("login-token").value || "").replace(/^Bearer\s+/i, "").trim();
      showLoginError("");
      function afterHostUnlock() {
        hideLogin();
        loadConfig().then(function () {
          refreshPin();
          connectEvents();
        });
      }
      function tryToken() {
        if (!t) {
          showLoginError("Enter the 6-digit PIN or host token");
          return;
        }
        fetch("/api/login", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          credentials: "same-origin",
          body: JSON.stringify({ token: t }),
        }).then(function (r) { return r.json().then(function (b) { return { status: r.status, b: b }; }); }).then(function (x) {
          if (x.status !== 200 || !x.b.ok) {
            showLoginError(x.b.error || "bad token");
            return;
          }
          setCookie("streamaid_token", t);
          afterHostUnlock();
        }).catch(function (err) {
          showLoginError(String(err && err.message || err));
        });
      }
      if (pin) {
        fetch("/api/otp/redeem", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          credentials: "same-origin",
          body: JSON.stringify({ pin: pin }),
        }).then(function (r) { return r.json().then(function (b) { return { status: r.status, b: b }; }); }).then(function (x) {
          if (x.status !== 200 || !x.b.session) {
            if (t) {
              tryToken();
              return;
            }
            showLoginError(x.b.error || (x.status === 429 ? "too many tries" : "bad PIN"));
            return;
          }
          setCookie("streamaid_session", x.b.session);
          setCookie("streamaid_viewer", "1");
          hideLogin();
          showH264();
        }).catch(function (err) {
          if (t) tryToken();
          else showLoginError(String(err && err.message || err));
        });
        return;
      }
      tryToken();
    });
  }
  function refreshPin() {
    api("/api/otp").then(function (r) { return r.json(); }).then(function (b) {
      const el = $("pin-pill");
      if (el && b.pin) el.textContent = "PIN " + b.pin;
    }).catch(function () {});
  }
  refreshPin();
  setInterval(refreshPin, 15000);
  const gear = $("gear");
  if (gear) {
    gear.addEventListener("click", function (ev) {
      ev.stopPropagation();
      if (isDrawerOpen()) closeDrawer();
      else openDrawer();
    });
  }
  const drawerClose = $("drawer-close");
  if (drawerClose) {
    drawerClose.addEventListener("click", function () { closeDrawer(); });
  }
  const backdrop = $("drawer-backdrop");
  if (backdrop) {
    backdrop.addEventListener("click", function () { closeDrawer(); });
  }
  document.addEventListener("keydown", function (ev) {
    if (ev.key !== "Escape") return;
    if (!isDrawerOpen()) return;
    ev.preventDefault();
    ev.stopPropagation();
    closeDrawer();
  }, true);
  const jpeg = $("cfg-jpeg");
  if (jpeg) {
    jpeg.addEventListener("input", function () {
      const jv = $("jpeg-val");
      if (jv) jv.textContent = jpeg.value;
    });
  }
  ["cfg-llm-enabled", "cfg-ai-enabled", "cfg-control-enabled", "cfg-audio"].forEach(function (id) {
    const el = $(id);
    if (!el) return;
    el.addEventListener("change", function () { syncFeatureUi(); });
  });
  const modeEl = $("cfg-mode");
  if (modeEl) modeEl.addEventListener("change", function () { syncFeatureUi(); });
  const unmute = $("unmute");
  if (unmute) {
    unmute.addEventListener("click", function () {
      const video = $("stream-video");
      if (!video) return;
      video.muted = false;
      video.volume = 1;
      video.play().catch(function () {});
      unmute.textContent = "Mute";
    });
  }
  const save = $("save");
  if (save) {
    save.addEventListener("click", async function () {
      const form = readConfigForm();
      save.disabled = true;
      try {
        const res = await api("/api/config", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(form),
        });
        const r = await res.json();
        const note = $("save-note");
        if (note) note.textContent = r.note || "saved";
        const rn = $("restart-note");
        if (rn) rn.classList.toggle("hidden", !r.restart_required);
        if (form.token) setCookie("streamaid_token", form.token);
        if (r.applied) closeDrawer();
        if (r.applied && (form.encoder.mode !== mode || !!(form.capture && form.capture.audio) !== !!(cfg && cfg.capture && cfg.capture.audio))) {
          setTimeout(function () { location.reload(); }, 400);
          return;
        }
        await loadConfig();
      } catch (err) {
        const note = $("save-note");
        if (note) note.textContent = "error: " + err.message;
      } finally {
        save.disabled = false;
      }
    });
  }
  const detect = $("detect");
  if (detect) {
    detect.addEventListener("click", function () { loadDisplays(); });
  }
  const disp = $("cfg-display");
  if (disp) {
    disp.addEventListener("change", function () {
      const input = $("cfg-input");
      if (input) input.value = disp.value;
    });
  }
  const pill = $("display-pill");
  if (pill) {
    pill.addEventListener("change", function () {
      const input = $("cfg-input");
      if (input) input.value = pill.value;
      if (typeof fetch !== "function") return;
      api("/api/config", {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ capture: { input: pill.value } }),
      }).catch(function () {});
    });
  }
  bindControl($("stream-video"));
  bindControl($("stream-canvas"));
  function bindFileDrop(el) {
    if (!el || el.dataset.fileBound) return;
    el.dataset.fileBound = "1";
    el.addEventListener("dragover", function (ev) {
      if (!featureFlags().ctl) return;
      ev.preventDefault();
      el.classList.add("drop-hover");
    });
    el.addEventListener("dragleave", function () { el.classList.remove("drop-hover"); });
    el.addEventListener("drop", function (ev) {
      el.classList.remove("drop-hover");
      if (!featureFlags().ctl) return;
      ev.preventDefault();
      uploadDroppedFiles(ev.dataTransfer && ev.dataTransfer.files);
    });
  }
  bindFileDrop($("file-drop"));
  bindFileDrop($("stream-pane"));
  const fileInput = $("file-input");
  if (fileInput) {
    fileInput.addEventListener("change", function () {
      uploadDroppedFiles(fileInput.files);
      fileInput.value = "";
    });
  }
  function sendKeyEvent(ev, down) {
    if (isDrawerOpen()) return;
    if (ev.target && (ev.target.tagName === "INPUT" || ev.target.tagName === "TEXTAREA")) return;
    if (!featureFlags().ctl) return;
    const mods = eventMods(ev);
    const accel = ev.metaKey || ev.ctrlKey || ev.altKey;
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
    if (isDrawerOpen()) return;
    if (!featureFlags().ctl) return;
    if (ev.target && (ev.target.tagName === "INPUT" || ev.target.tagName === "TEXTAREA")) return;
    const files = ev.clipboardData && ev.clipboardData.files;
    if (clipboardHasNonImageFiles(files)) {
      ev.preventDefault();
      uploadDroppedFiles(files);
      return;
    }
    const items = ev.clipboardData && ev.clipboardData.items;
    if (items) {
      for (let i = 0; i < items.length; i++) {
        if (items[i].type && items[i].type.indexOf("image/") === 0) {
          const f = items[i].getAsFile();
          if (!f) continue;
          ev.preventDefault();
          pasteImageFile(f);
          return;
        }
      }
    }
    const text = ev.clipboardData && ev.clipboardData.getData("text/plain");
    if (!text) return;
    ev.preventDefault();
    sendControl("paste", { text: text });
  });
  const cancelAi = $("cu-cancel");
  if (cancelAi) {
    cancelAi.addEventListener("click", async function () {
      try {
        await fetch(url("/api/computer-use/cancel"), { method: "POST" });
        const out = $("cu-out");
        if (out) out.textContent = "cancelled";
      } catch (err) { /* ignore */ }
    });
  }
  const cu = $("cu-form");
  if (cu) {
    cu.addEventListener("submit", async function (e) {
      e.preventDefault();
      const task = ($("cu-task") && $("cu-task").value || "").trim();
      if (!task) return;
      const out = $("cu-out");
      if (out) out.textContent = "running…";
      try {
        const res = await fetch(url("/api/computer-use"), {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ task: task }),
        });
        const body = await res.json();
        if (out) {
          if (res.status === 403) out.textContent = "host has AI control off";
          else out.textContent = body.error ? ("error: " + body.error) : "ok";
        }
      } catch (err) {
        if (out) out.textContent = "error: " + err.message;
      }
    });
  }
  const ask = $("ask-form");
  if (ask) {
    ask.addEventListener("submit", async function (e) {
      e.preventDefault();
      const q = ($("ask-input") && $("ask-input").value || "").trim();
      if (!q) return;
      const box = $("ask-result");
      if (box) box.textContent = "asking…";
      const href = workerHttp("/api/ask") || url("/api/ask");
      try {
        const res = await fetch(href, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ question: q }),
        });
        const body = await res.json();
        if (box) {
          box.textContent = body.error
            ? ("error: " + body.error)
            : ((body.answer || "") + (body.confidence != null ? " (" + body.confidence + "%)" : ""));
        }
      } catch (err) {
        if (box) box.textContent = "error: " + err.message;
      }
    });
  }
  loadConfig().then(function () {
    refreshEdgeAnalysis();
    setInterval(refreshEdgeAnalysis, 4000);
  });
  connectEvents();
  (function loopMapThumbs() {
    paintMapThumbs();
    if (typeof requestAnimationFrame === "function") requestAnimationFrame(loopMapThumbs);
    else setTimeout(loopMapThumbs, 33);
  })();
  api("/api/status").then(function (res) { return res.json(); }).then(renderStatus).catch(function () {});
}

if (typeof window !== "undefined") {
  window.streamaidUi = {
    closeDrawer: closeDrawer,
    openDrawer: openDrawer,
    isDrawerOpen: isDrawerOpen,
    setDrawerOpen: setDrawerOpen,
    syncFeatureUi: syncFeatureUi,
    syncEncoderUi: syncEncoderUi,
    featureFlags: featureFlags,
    layoutDisplayMap: layoutDisplayMap,
    paintMapThumbs: paintMapThumbs,
    currentToken: currentToken,
    url: url,
  };
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", onReady);
  } else if (typeof document.addEventListener === "function") {
    onReady();
  }
}
