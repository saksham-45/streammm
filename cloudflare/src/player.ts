/** Public watch page served at GET /. Token is taken from ?token=. */

export const PLAYER_HTML = `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>streamaid live</title>
<style>
  :root { color-scheme: dark; }
  body { margin: 0; background: #111; color: #ddd; font-family: system-ui, sans-serif; }
  header { padding: 10px 16px; display: flex; gap: 12px; align-items: center; border-bottom: 1px solid #333; }
  h1 { font-size: 16px; margin: 0; letter-spacing: 1px; }
  #pill { font-size: 13px; border: 1px solid #444; border-radius: 999px; padding: 4px 10px; }
  video { width: 100%; max-height: 90vh; background: #000; display: block; }
  #err { color: #f88; padding: 12px 16px; }
</style>
</head>
<body>
<header>
  <h1>streamaid</h1>
  <div id="pill">connecting…</div>
</header>
<video id="v" autoplay muted playsinline></video>
<div id="err"></div>
<script>
(function () {
  var TYPE_INIT = 1, TYPE_FRAG = 2, TYPE_JPEG = 3;
  var LIVE = 0.45, CAP = 2;
  var token = new URLSearchParams(location.search).get("token") || "";
  var pill = document.getElementById("pill");
  var err = document.getElementById("err");
  var video = document.getElementById("v");
  if (!token) {
    pill.textContent = "missing token";
    err.textContent = "Add ?token=… to this URL (from the stream host Settings → Cloudflare watch URL).";
    return;
  }
  if (!("MediaSource" in window)) {
    err.textContent = "This browser cannot play the live fMP4 stream.";
    return;
  }
  var ms, sb, ws, pending = [], liveSeeked = false;
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
  function pump() {
    if (!sb || sb.updating) return;
    if (!liveSeeked && sb.buffered.length) {
      var start = sb.buffered.start(0);
      var end = sb.buffered.end(sb.buffered.length - 1);
      if (end - start >= 0.2) {
        liveSeeked = true;
        video.currentTime = Math.max(start, end - LIVE);
        video.play().catch(function () {});
      }
    }
    if (sb.buffered.length && video.currentTime) {
      var lead = sb.buffered.end(sb.buffered.length - 1) - video.currentTime;
      video.playbackRate = lead > 1 ? 1.08 : 1;
    }
    if (!pending.length) return;
    var item = pending.shift();
    if (item.kind === TYPE_INIT) {
      try {
        if (sb.buffered.length) sb.remove(0, sb.buffered.end(sb.buffered.length - 1));
      } catch (e) {}
      liveSeeked = false;
      video.currentTime = 0;
      if (sb.buffered.length) { pending.unshift(item); return; }
    }
    try { sb.appendBuffer(item.chunk); } catch (e) { start(); }
  }
  function start() {
    if (ws) { try { ws.close(); } catch (e) {} }
    ms = new MediaSource();
    video.src = URL.createObjectURL(ms);
    ms.addEventListener("sourceopen", function () {
      var types = [
        'video/mp4; codecs="avc1.640028"',
        'video/mp4; codecs="avc1.42E01E"',
        "video/mp4"
      ];
      var type = types.find(function (t) { return MediaSource.isTypeSupported(t); });
      if (!type) { err.textContent = "H.264 MSE unsupported"; return; }
      sb = ms.addSourceBuffer(type);
      sb.addEventListener("updateend", pump);
      ws = new WebSocket(wsUrl());
      ws.binaryType = "arraybuffer";
      ws.onopen = function () { pill.textContent = "live"; };
      ws.onclose = function () {
        pill.textContent = "reconnecting";
        setTimeout(start, 1000);
      };
      ws.onmessage = function (ev) {
        var buf = new Uint8Array(ev.data);
        if (!buf.length) return;
        var type = buf[0];
        var payload = buf.slice(1);
        if (type === TYPE_INIT || type === TYPE_FRAG) {
          enqueue(type, payload);
          pump();
        }
      };
    });
  }
  start();
})();
</script>
</body>
</html>
`;
