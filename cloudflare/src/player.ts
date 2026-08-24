/** Public watch page served at GET /. Token is taken from ?token=. */

export const PLAYER_HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>streamaid live</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; background: #111; color: #ddd; font-family: system-ui, sans-serif; }
  header { padding: 10px 16px; display: flex; gap: 12px; align-items: center; border-bottom: 1px solid #333; }
  h1 { font-size: 16px; margin: 0; letter-spacing: 1px; }
  #pill { font-size: 13px; border: 1px solid #444; border-radius: 999px; padding: 4px 10px; }
  video, canvas { width: 100%; max-height: 90vh; background: #000; display: block; }
  canvas { display: none; }
  #err { color: #f88; padding: 12px 16px; }
  #tap {
    display: none; position: fixed; inset: 0; background: rgba(0,0,0,0.55);
    color: #fff; font-size: 18px; align-items: center; justify-content: center;
  }
</style>
</head>
<body>
<header>
  <h1>streamaid</h1>
  <div id="pill">connecting…</div>
</header>
<video id="v" autoplay muted playsinline webkit-playsinline></video>
<canvas id="c"></canvas>
<div id="err"></div>
<div id="tap">Tap to play</div>
<script>
(function () {
  var TYPE_INIT = 1, TYPE_FRAG = 2, TYPE_JPEG = 3;
  var LIVE = 0.45, CAP = 2;
  var token = new URLSearchParams(location.search).get("token") || "";
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

  if (!token) {
    pill.textContent = "missing token";
    err.textContent = "Add ?token=… to this URL (from the stream host Settings → Cloudflare watch URL).";
    return;
  }

  var MS = mediaSourceCtor();
  var ms, sb, ws, pending = [], liveSeeked = false, jpegMode = false, reconnectTimer = null;

  function wsUrl() {
    var u = new URL("/watch", location.href);
    u.protocol = u.protocol.replace("http", "ws");
    u.searchParams.set("token", token);
    return u.toString();
  }
  function enqueue(kind, chunk) {
    if (pending.length >= CAP) pending.shift();
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
      startMse();
    }
  }

  function openWs(onBinary) {
    if (ws) { try { ws.close(); } catch (e) {} }
    ws = new WebSocket(wsUrl());
    ws.binaryType = "arraybuffer";
    ws.onopen = function () { pill.textContent = jpegMode ? "live (jpeg)" : "live"; };
    ws.onclose = function () {
      pill.textContent = "reconnecting";
      if (reconnectTimer) return;
      reconnectTimer = setTimeout(function () {
        reconnectTimer = null;
        if (jpegMode) startJpeg(); else startMse();
      }, 1000);
    };
    ws.onmessage = function (ev) {
      var buf = new Uint8Array(ev.data);
      if (!buf.length) return;
      onBinary(buf[0], buf.slice(1));
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
      openWs(function (typeByte, payload) {
        if (typeByte === TYPE_JPEG) { drawJpeg(payload); return; }
        if (typeByte === TYPE_INIT || typeByte === TYPE_FRAG) {
          enqueue(typeByte, payload);
          pump();
        }
      });
    }
    ms.addEventListener("sourceopen", onOpen);
    ms.addEventListener("sourceended", function () {});
  }

  startMse();
})();
</script>
</body>
</html>
`;
