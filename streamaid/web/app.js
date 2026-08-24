"use strict";

const $ = (id) => document.getElementById(id);

// ---- token / URL helpers ----

function getCookie(name) {
  const esc = name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const m = document.cookie.match(new RegExp("(?:^|; )" + esc + "=([^;]*)"));
  return m ? decodeURIComponent(m[1]) : "";
}
function setCookie(name, value) {
  document.cookie = name + "=" + encodeURIComponent(value) + "; path=/";
}
const token = getCookie("streamaid_token");
function url(path) {
  return token
    ? path + (path.includes("?") ? "&" : "?") + "token=" + encodeURIComponent(token)
    : path;
}

// ---- state ----

let cfg = null;
let mode = "mjpeg";
let eventSource = null;
let ms = null, sb = null, reader = null, pending = [];
let mseReconnectTimer = null;
let liveSeeked = false;

// ---- fetch helper with 401 handling ----

async function api(path, opts) {
  const res = await fetch(path, opts);
  if (res.status === 401) {
    showLogin();
    throw new Error("unauthorized");
  }
  return res;
}

// ---- login overlay ----

function showLogin() {
  $("login-overlay").classList.remove("hidden");
}
$("login-form").addEventListener("submit", (e) => {
  e.preventDefault();
  const t = $("login-token").value.trim();
  if (!t) return;
  setCookie("streamaid_token", t);
  location.reload();
});

// ---- status pill ----

function renderStatus(s) {
  const cap = s.capture || {};
  const st = s.stream || {};
  const llm = s.llm || {};
  const q = s.quality || {};
  const fps = (cap.fps_actual || 0).toFixed(1);
  const label = st.mode === "ffmpeg" ? "h264" : (st.mode === "hevc" ? "hevc" : "mjpeg");
  const n = st.clients || 0;
  const el = $("status-pill");
  el.textContent = fps + " fps · " + label + " · " + n + " client" + (n === 1 ? "" : "s");
  el.classList.toggle("error", !!(cap.error || llm.last_error));
  el.title = (cap.error ? "capture: " + cap.error + " " : "") + (llm.last_error ? "llm: " + llm.last_error : "");
  const qp = $("quality-pill");
  if (q.last_check_at) {
    const when = fmtTs(q.last_check_at).slice(11, 19);
    qp.textContent = "quality " + (q.score || 0) + "% " + (q.ok ? "✓" : "⚠") + " " + when;
    qp.classList.toggle("error", !q.ok);
    qp.title = "checked every 3 min · sharpness " + q.sharpness + "% · readability " +
      q.readability + "%" + (q.ocr_confidence != null ? " · OCR " + q.ocr_confidence + "% (" + q.ocr_words + " words)" : "");
  } else {
    qp.textContent = "quality —";
  }
}

// ---- analysis rendering ----

function renderAnalysis(a) {
  $("latest-summary").textContent = a.summary || "(no summary)";
  const wrap = $("questions");
  wrap.innerHTML = "";
  for (const q of a.questions || []) {
    const div = document.createElement("div");
    div.className = "question";
    const qEl = document.createElement("div");
    qEl.className = "q";
    qEl.textContent = q.question || "(question)";
    const aEl = document.createElement("div");
    aEl.className = "a";
    aEl.textContent = q.answer || "(no answer)";
    const bar = document.createElement("div");
    bar.className = "conf";
    const fill = document.createElement("div");
    fill.className = "conf-fill";
    fill.style.width = Math.max(0, Math.min(100, q.confidence || 0)) + "%";
    bar.appendChild(fill);
    const label = document.createElement("span");
    label.className = "conf-label";
    label.textContent = "confidence " + (q.confidence || 0) + "%";
    const det = document.createElement("details");
    const sum = document.createElement("summary");
    sum.textContent = "reasoning";
    det.appendChild(sum);
    det.appendChild(document.createTextNode(q.reasoning || ""));
    div.append(qEl, aEl, bar, label, det);
    wrap.appendChild(div);
  }
}

function prependHistory(a) {
  const ul = $("history");
  const li = document.createElement("li");
  li.textContent = fmtTs(a.ts) + " — " + (a.summary || "");
  ul.prepend(li);
}
function renderHistory(list) {
  $("history").innerHTML = "";
  for (const a of list || []) prependHistory(a);
}
function fmtTs(ts) {
  return String(ts || "").replace("T", " ").slice(0, 19);
}

// ---- ask ----

$("ask-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  const q = $("ask-input").value.trim();
  if (!q) return;
  const box = $("ask-result");
  box.textContent = "asking…";
  try {
    const res = await api("/api/ask", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: q }),
    });
    const body = await res.json();
    if (!res.ok) {
      box.textContent = "error: " + (body.error || res.status);
      return;
    }
    box.innerHTML = "";
    const ans = document.createElement("div");
    ans.className = "a";
    ans.textContent = body.answer;
    const conf = document.createElement("div");
    conf.className = "conf-label";
    conf.textContent = "confidence " + body.confidence + "%";
    const det = document.createElement("details");
    const sum = document.createElement("summary");
    sum.textContent = "reasoning";
    det.appendChild(sum);
    det.appendChild(document.createTextNode(body.reasoning || ""));
    box.append(ans, conf, det);
  } catch (err) {
    box.textContent = "error: " + err.message;
  }
});

// ---- config drawer ----

$("gear").addEventListener("click", () => {
  const d = $("config-drawer");
  d.classList.toggle("hidden");
  if (!d.classList.contains("hidden") && cfg) fillConfigForm(cfg);
});
$("cfg-jpeg").addEventListener("input", () => {
  $("jpeg-val").textContent = $("cfg-jpeg").value;
});

function fillConfigForm(c) {
  $("cfg-host").value = c.host;
  $("cfg-port").value = c.port;
  $("cfg-token").value = c.token;
  $("cfg-input").value = c.capture.input;
  $("cfg-fps").value = c.capture.fps;
  $("cfg-scale").value = String(c.capture.scale);
  $("cfg-jpeg").value = c.capture.jpeg_quality;
  $("jpeg-val").textContent = c.capture.jpeg_quality;
  $("cfg-mode").value = c.encoder.mode;
  $("cfg-bitrate").value = c.encoder.bitrate_kbps;
  $("cfg-llm-enabled").checked = c.llm.enabled;
  $("cfg-base-url").value = c.llm.base_url;
  $("cfg-api-key").value = c.llm.api_key;
  $("cfg-model").value = c.llm.model;
  $("cfg-interval").value = c.llm.interval_sec;
  $("cfg-prompt").value = c.llm.prompt;
}

function readConfigForm() {
  return {
    host: $("cfg-host").value,
    port: parseInt($("cfg-port").value, 10) || 8080,
    token: $("cfg-token").value,
    capture: {
      driver: "ffmpeg",
      input: $("cfg-input").value,
      fps: parseInt($("cfg-fps").value, 10) || 30,
      scale: parseFloat($("cfg-scale").value) || 1.0,
      jpeg_quality: parseInt($("cfg-jpeg").value, 10) || 80,
    },
    encoder: {
      mode: $("cfg-mode").value,
      bitrate_kbps: parseInt($("cfg-bitrate").value, 10) || 8000,
    },
    llm: {
      enabled: $("cfg-llm-enabled").checked,
      base_url: $("cfg-base-url").value,
      api_key: $("cfg-api-key").value,
      model: $("cfg-model").value,
      interval_sec: parseInt($("cfg-interval").value, 10) || 5,
      prompt: $("cfg-prompt").value,
    },
  };
}

$("save").addEventListener("click", async () => {
  const btn = $("save");
  const form = readConfigForm();
  btn.disabled = true;
  try {
    const res = await api("/api/config", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(form),
    });
    const r = await res.json();
    $("save-note").textContent = r.note || "saved";
    $("restart-note").classList.toggle("hidden", !r.restart_required);
    if (!res.ok) {
      $("save-note").textContent = "error: " + (r.error || res.status);
      return;
    }
    if (r.applied && form.encoder.mode !== mode) {
      setTimeout(() => location.reload(), 500); // swap img <-> video cleanly
      return;
    }
    await loadConfig();
  } catch (err) {
    $("save-note").textContent = "error: " + err.message;
  } finally {
    btn.disabled = false;
  }
});

$("detect").addEventListener("click", async () => {
  try {
    const res = await api("/api/capture-devices");
    const devices = await res.json();
    const sel = $("device-select");
    sel.innerHTML = "";
    for (const d of devices) {
      const opt = document.createElement("option");
      opt.value = d.id;
      opt.textContent = d.name + " (" + d.id + ")";
      sel.appendChild(opt);
    }
    sel.classList.toggle("hidden", devices.length === 0);
    sel.onchange = () => { $("cfg-input").value = sel.value; };
  } catch (err) { /* ignore */ }
});

// ---- stream elements ----

const img = $("stream-img");
const video = $("stream-video");
const canvas = $("stream-canvas");
const FORCE_CANVAS = new URLSearchParams(location.search).get("canvas") === "1";

function isIOS() {
  const ua = navigator.userAgent;
  return /iPad|iPhone|iPod/.test(ua) ||
    (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
}

function showMjpeg() {
  video.pause();
  video.removeAttribute("src");
  canvas.classList.add("hidden");
  if (isIOS() || FORCE_CANVAS) {
    // iOS Safari does not render multipart/x-mixed-replace in <img>; decode
    // frames from the fetch stream onto a canvas instead.
    img.classList.add("hidden");
    canvas.classList.remove("hidden");
    renderMjpegCanvas();
  } else {
    img.classList.remove("hidden");
    img.src = url("/stream.mjpeg");
  }
}
img.addEventListener("error", () => {
  // stream drops on capture restart; reconnect
  setTimeout(() => { img.src = url("/stream.mjpeg"); }, 2000);
});

// ---- iOS-safe MJPEG renderer (fetch + canvas, no <img> multipart) ----

const BOUNDARY = new TextEncoder().encode("--frame\r\n");

function renderMjpegCanvas() {
  const ctx = canvas.getContext("2d");
  if (!ctx) { showStreamError("canvas unsupported"); return; }
  fetch(url("/stream.mjpeg"))
    .then((res) => {
      if (!res.ok || !res.body) {
        if (res.status === 401) { showLogin(); return; }
        showStreamError("stream error " + res.status);
        setTimeout(renderMjpegCanvas, 3000);
        return;
      }
      const reader = res.body.getReader();
      let buf = new Uint8Array(0);
      let decoding = false;
      let drawW = 0, drawH = 0;

      function next() {
        reader.read().then(({ done, value }) => {
          if (done) { setTimeout(renderMjpegCanvas, 2000); return; }
          const tmp = new Uint8Array(buf.length + value.length);
          tmp.set(buf);
          tmp.set(value, buf.length);
          buf = tmp;
          for (;;) {
            const start = indexOfBytes(buf, BOUNDARY);
            if (start > 0) {
              // resync: discard bytes before the next boundary
              buf = buf.slice(start);
              continue;
            }
            if (start === -1) break; // wait for more data
            // buf starts with the boundary; find the header terminator
            const hEnd = indexOfBytes(buf, new Uint8Array([13, 10, 13, 10]));
            if (hEnd === -1) break;
            const headers = new TextDecoder().decode(buf.slice(BOUNDARY.length, hEnd));
            const m = /Content-Length:\s*(\d+)/i.exec(headers);
            if (!m) break;
            const clen = parseInt(m[1], 10);
            const jpegStart = hEnd + 4;
            if (buf.length < jpegStart + clen) break;
            const jpeg = buf.slice(jpegStart, jpegStart + clen);
            buf = buf.slice(jpegStart + clen);
            if (!decoding) {
              decoding = true;
              decodeAndDraw(jpeg);
            }
          }
          if (buf.length > 20 * 1024 * 1024) buf = new Uint8Array(0);
          next();
        }).catch(() => setTimeout(renderMjpegCanvas, 3000));
      }

      function decodeAndDraw(jpeg) {
        const finish = (bmp) => {
          if (bmp) {
            if (bmp.width !== drawW || bmp.height !== drawH) {
              drawW = canvas.width = bmp.width;
              drawH = canvas.height = bmp.height;
            }
            ctx.drawImage(bmp, 0, 0, drawW, drawH);
            bmp.close && bmp.close();
          }
          decoding = false;
        };
        if (window.createImageBitmap) {
          createImageBitmap(new Blob([jpeg], { type: "image/jpeg" }))
            .then(finish)
            .catch(() => { decoding = false; });
        } else {
          const im = new Image();
          im.onload = () => finish(im);
          im.onerror = () => { decoding = false; };
          im.src = URL.createObjectURL(new Blob([jpeg], { type: "image/jpeg" }));
        }
      }

      next();
    })
    .catch(() => setTimeout(renderMjpegCanvas, 3000));
}

function indexOfBytes(haystack, needle) {
  outer: for (let i = 0; i + needle.length <= haystack.length; i++) {
    for (let j = 0; j < needle.length; j++) {
      if (haystack[i + j] !== needle[j]) continue outer;
    }
    return i;
  }
  return -1;
}

function showH264() {
  img.classList.add("hidden");
  canvas.classList.add("hidden");
  video.classList.remove("hidden");
  startMse();
}

function showStreamError(msg) {
  const el = $("stream-error");
  el.textContent = msg;
  el.classList.remove("hidden");
}

// ---- MediaSource H.264 client ----

function startMse() {
  if (!("MediaSource" in window)) {
    showStreamError("MediaSource unsupported — switch encoder to mjpeg");
    return;
  }
  teardownMse();
  ms = new MediaSource();
  video.src = URL.createObjectURL(ms);
  ms.addEventListener("sourceopen", onSourceOpen);
  video.addEventListener("error", onVideoError);
}

function teardownMse() {
  if (mseReconnectTimer) { clearTimeout(mseReconnectTimer); mseReconnectTimer = null; }
  if (reader) { try { reader.cancel(); } catch (e) { /* ignore */ } }
  if (ms && video.src.startsWith("blob:")) {
    try { URL.revokeObjectURL(video.src); } catch (e) { /* ignore */ }
  }
  if (sb) {
    try { sb.removeEventListener("updateend", pump); } catch (e) { /* ignore */ }
  }
  video.removeEventListener("timeupdate", pruneBuffer);
  video.removeEventListener("error", onVideoError);
  sb = null; ms = null; reader = null; pending = [];
  liveSeeked = false;
}

function onVideoError() {
  // A poisoned pipeline (bad append/decode) cannot recover in place.
  if (ms && video.error) rebuildMse();
}

function rebuildMse() {
  showStreamError("stream error — reconnecting");
  startMse();
}

function safeAppend(chunk) {
  try {
    sb.appendBuffer(chunk);
  } catch (e) {
    rebuildMse();
  }
}

function onSourceOpen() {
  const hevc = mode === "hevc";
  const candidates = hevc
    ? [
        'video/mp4; codecs="hvc1.1.6.L93.B0"',
        'video/mp4; codecs="hev1.1.6.L93.B0"',
        "video/mp4",
      ]
    : [
        'video/mp4; codecs="avc1.640028"',
        'video/mp4; codecs="avc1.42E01E"',
        "video/mp4",
      ];
  const type = candidates.find((t) => MediaSource.isTypeSupported(t));
  if (!type) {
    showStreamError(
      (hevc ? "HEVC" : "MP4") + " not supported by this browser — switch encoder to " +
      (hevc ? "H.264 or MJPEG" : "MJPEG")
    );
    return;
  }
  try {
    sb = ms.addSourceBuffer(type);
  } catch (e) {
    showStreamError("SourceBuffer error: " + e.message);
    return;
  }
  sb.addEventListener("updateend", pump);
  video.addEventListener("timeupdate", pruneBuffer);
  video.play().catch(() => {});
  fetchStream();
}

function fetchStream() {
  fetch(url("/stream.mp4"))
    .then((res) => {
      if (!res.ok || !res.body) {
        if (res.status === 401) { showLogin(); return; }
        showStreamError("stream error " + res.status + " — switch encoder to mjpeg");
        return;
      }
      reader = res.body.getReader();
      pump();
    })
    .catch(() => scheduleMseReconnect());
}

function pump() {
  if (!sb || sb.updating) return;
  maybeLiveSeek();
  watchPlayback();
  if (pending.length) {
    const chunk = pending.shift();
    if (isInit(chunk)) {
      handleNewStream();
      if (sb.buffered.length) { pending.unshift(chunk); return; } // await remove
      safeAppend(chunk);
      return;
    }
    safeAppend(chunk);
    return;
  }
  if (!reader) return;
  reader.read()
    .then(({ done, value }) => {
      if (done) { scheduleMseReconnect(); return; }
      if (sb.updating) { pending.push(value); return; }
      if (isInit(value)) {
        handleNewStream();
        if (sb.buffered.length) { pending.push(value); return; } // await remove
        safeAppend(value);
        return;
      }
      safeAppend(value);
    })
    .catch(() => scheduleMseReconnect());
}

function isInit(chunk) {
  return chunk.length >= 8 &&
    String.fromCharCode(chunk[4], chunk[5], chunk[6], chunk[7]) === "ftyp";
}

function handleNewStream() {
  // New capture stream (fresh load or capture restart): the media timeline
  // restarts at 0, so drop the old timeline and seek back to the start.
  try {
    if (sb.buffered.length) sb.remove(0, sb.buffered.end(sb.buffered.length - 1));
  } catch (e) { /* ignore */ }
  liveSeeked = false;
  video.currentTime = 0;
}
let lastCT = -1, lastCTAt = 0, seekDeadline = 0;
function maybeLiveSeek() {
  // Joining mid-stream: the media timeline starts at the capture's origin,
  // so a new client's first samples may be N seconds in. Seek once to a
  // point ~2.5s behind the live edge: close enough to feel live, far
  // enough from the appending tail to seek safely, with buffer to ride
  // out internet jitter (Twitch-class smoothness).
  if (liveSeeked || !sb || !sb.buffered.length) return;
  if (video.currentTime < sb.buffered.start(0) - 0.1) {
    liveSeeked = true;
    video.currentTime = Math.max(0, sb.buffered.end(sb.buffered.length - 1) - 2.5);
    seekDeadline = performance.now() + 2500;
    video.play().catch(() => {});
  }
}

function watchPlayback() {
  // Self-healing watchdog, called on every fragment append. Recovers from
  // Chrome's hung-seek stall (seek aimed at an unstable buffer tail) and
  // from playback that freezes with data still buffered ahead.
  if (!sb || !sb.buffered.length) return;
  const now = performance.now();
  const ct = video.currentTime;
  const end = sb.buffered.end(sb.buffered.length - 1);
  if (video.seeking) {
    if (seekDeadline && now > seekDeadline) {
      video.currentTime = sb.buffered.start(0) + 0.1;
      seekDeadline = now + 2500;
    }
    return;
  }
  if (ct === lastCT) {
    if (now - lastCTAt > 3000 && end - ct > 2) {
      video.currentTime = end - 2.5;
      seekDeadline = now + 2500;
    }
  } else {
    lastCT = ct;
    lastCTAt = now;
  }
}

function scheduleMseReconnect() {
  if (mseReconnectTimer) return;
  mseReconnectTimer = setTimeout(() => {
    mseReconnectTimer = null;
    fetchStream();
  }, 3000);
}

function pruneBuffer() {
  if (!sb || sb.updating || !video.buffered.length) return;
  const end = video.buffered.end(video.buffered.length - 1);
  if (end - video.currentTime > 2.5 && video.currentTime > 1) {
    // drop only played-out data, never the playhead's own region
    try { sb.remove(0, video.currentTime - 1); } catch (e) { /* ignore */ }
  }
}

// ---- config load + events ----

async function loadConfig() {
  try {
    const res = await api("/api/config");
    cfg = await res.json();
  } catch (e) {
    return;
  }
  mode = cfg.encoder.mode;
  $("analysis-banner").classList.toggle("hidden", !cfg.llm.enabled);
  if (mode === "mjpeg") showMjpeg();
  else showH264();
}

function connectEvents() {
  if (eventSource) eventSource.close();
  eventSource = new EventSource(url("/api/events"));
  eventSource.addEventListener("status", (e) => {
    try { renderStatus(JSON.parse(e.data)); } catch (err) { /* ignore */ }
  });
  eventSource.addEventListener("analysis", (e) => {
    try {
      const a = JSON.parse(e.data);
      renderAnalysis(a);
      prependHistory(a);
    } catch (err) { /* ignore */ }
  });
  eventSource.addEventListener("config-applied", () => loadConfig());
  eventSource.onerror = () => {
    eventSource.close();
    eventSource = null;
  };
}

// fallback polling when SSE is down
setInterval(async () => {
  if (eventSource && eventSource.readyState === EventSource.OPEN) return;
  try {
    const res = await api("/api/status");
    renderStatus(await res.json());
  } catch (e) { /* ignore */ }
}, 2000);

window.addEventListener("DOMContentLoaded", async () => {
  await loadConfig();
  connectEvents();
  try {
    const res = await api("/api/status");
    renderStatus(await res.json());
  } catch (e) { /* ignore */ }
  try {
    const res = await api("/api/analysis");
    renderHistory(await res.json());
  } catch (e) { /* ignore */ }
});
