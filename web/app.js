"use strict";

const LIVE_EDGE_S = 0.45;
const CATCHUP_LEAD_S = 1.0;
const CATCHUP_RATE = 1.08;
const PENDING_CAP = 2;
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
  document.cookie = name + "=" + encodeURIComponent(value) + "; path=/";
}
const token = getCookie("streamaid_token");
function hasViewerSession() {
  return getCookie("streamaid_viewer") === "1" || !!getCookie("streamaid_session");
}
function url(path) {
  let out = path;
  if (token) {
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

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (res.status === 401) {
    if (!hasViewerSession()) showLogin();
    throw new Error("unauthorized");
  }
  return res;
}

function showLogin() {
  const el = $("login-overlay");
  if (el) el.classList.remove("hidden");
}
function hideLogin() {
  const el = $("login-overlay");
  if (el) el.classList.add("hidden");
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

function bindControl(el) {
  if (!el || el.dataset.ctlBound) return;
  el.dataset.ctlBound = "1";
  el.addEventListener("click", function (ev) {
    sendControl("click", normEvent(el, ev));
  });
  el.addEventListener("mousemove", function (ev) {
    if (!ev.buttons) return;
    sendControl("move", normEvent(el, ev));
  });
  el.addEventListener("wheel", function (ev) {
    ev.preventDefault();
    const p = normEvent(el, ev);
    sendControl("scroll", { x: p.x, y: p.y, dy: ev.deltaY });
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
  if (pending.length >= PENDING_CAP) pending.shift();
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
  try { sb.appendBuffer(chunk); } catch (e) { rebuildMse(); }
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

function onWsMessage(ev) {
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
  const candidates = hevc
    ? ['video/mp4; codecs="hvc1.1.6.L93.B0"', 'video/mp4; codecs="hev1.1.6.L93.B0"', "video/mp4"]
    : [
        'video/mp4; codecs="avc1.64001F"',
        'video/mp4; codecs="avc1.640028"',
        'video/mp4; codecs="avc1.4D401F"',
        'video/mp4; codecs="avc1.42E01E"',
        "video/mp4",
      ];
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

function fillConfigForm(c) {
  if (!c) return;
  function set(id, val) { const el = $(id); if (el) el.value = val; }
  set("cfg-host", c.host);
  set("cfg-port", c.port);
  set("cfg-token", c.token);
  set("cfg-input", c.capture && c.capture.input || "");
  set("cfg-fps", c.capture && c.capture.fps);
  set("cfg-scale", String(c.capture && c.capture.scale));
  set("cfg-jpeg", c.capture && c.capture.jpeg_quality);
  const jv = $("jpeg-val");
  if (jv) jv.textContent = c.capture && c.capture.jpeg_quality;
  set("cfg-mode", c.encoder && c.encoder.mode);
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
  set("cfg-base-url", c.llm && c.llm.base_url || "");
  set("cfg-api-key", c.llm && c.llm.api_key || "");
  set("cfg-model", c.llm && c.llm.model || "");
  set("cfg-interval", c.llm && c.llm.interval_sec);
  set("cfg-prompt", c.llm && c.llm.prompt || "");
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
  const banner = $("analysis-banner");
  if (banner) banner.classList.toggle("hidden", !(cfg.llm && cfg.llm.enabled));
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
  const login = $("login-form");
  if (login) {
    login.addEventListener("submit", function (e) {
      e.preventDefault();
      const pin = ($("login-pin") && $("login-pin").value || "").trim();
      const t = ($("login-token") && $("login-token").value || "").trim();
      if (pin && pin.length === 6) {
        fetch("/api/otp/redeem", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ pin: pin }),
        }).then(function (r) { return r.json(); }).then(function (b) {
          if (b.session) {
            setCookie("streamaid_session", b.session);
            setCookie("streamaid_viewer", "1");
            hideLogin();
            showH264();
          }
        }).catch(function () {});
        return;
      }
      if (!t) return;
      setCookie("streamaid_token", t);
      location.reload();
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
    gear.addEventListener("click", function () {
      const d = $("config-drawer");
      if (!d) return;
      d.classList.toggle("hidden");
      if (!d.classList.contains("hidden") && cfg) fillConfigForm(cfg);
    });
  }
  const jpeg = $("cfg-jpeg");
  if (jpeg) {
    jpeg.addEventListener("input", function () {
      const jv = $("jpeg-val");
      if (jv) jv.textContent = jpeg.value;
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
        if (r.applied && form.encoder.mode !== mode) {
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
    detect.addEventListener("click", async function () {
      try {
        const res = await api("/api/capture-devices");
        const devices = await res.json();
        const sel = $("device-select");
        if (!sel) return;
        sel.innerHTML = "";
        devices.forEach(function (d) {
          const opt = document.createElement("option");
          opt.value = d.id;
          opt.textContent = d.name + " (" + d.id + ")";
          sel.appendChild(opt);
        });
        sel.classList.toggle("hidden", devices.length === 0);
        sel.onchange = function () { $("cfg-input").value = sel.value; };
      } catch (err) { /* ignore */ }
    });
  }
  bindControl($("stream-video"));
  bindControl($("stream-canvas"));
  document.addEventListener("keydown", function (ev) {
    if (ev.target && (ev.target.tagName === "INPUT" || ev.target.tagName === "TEXTAREA")) return;
    if (ev.key.length === 1) sendControl("type", { text: ev.key });
    else sendControl("key", { key: ev.key });
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
  api("/api/status").then(function (res) { return res.json(); }).then(renderStatus).catch(function () {});
}

if (typeof document !== "undefined") {
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", onReady);
  } else if (typeof document.addEventListener === "function") {
    onReady();
  }
}
