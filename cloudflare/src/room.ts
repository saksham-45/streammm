import { DurableObject } from "cloudflare:workers";
import {
  ANALYZE_PROMPT,
  analysisFromModel,
  askFromModel,
  askPrompt,
  completeVision,
  DEFAULT_BASE,
  DEFAULT_MODEL,
  parseJsonObject,
  type Analysis,
} from "./llm";

export type ConnRole = "publisher" | "viewer";

export interface Env {
  STREAM_ROOM: DurableObjectNamespace;
  STREAM_TOKEN?: string;
  EDGE_PUBLIC?: string;
  DEEPSEEK_API_KEY?: string;
  DEEPSEEK_BASE_URL?: string;
  DEEPSEEK_MODEL?: string;
}

type Attachment = { role: ConnRole; session?: string };

const CLOSE_REPLACED = 4000;
const TYPE_INIT = 1;
const TYPE_FRAG = 2;
const TYPE_SNAP = 4;
const FAIL_LIMIT = 5;
const LOCKOUT_MS = 30_000;
const SESSION_TTL_MS = 86_400_000;

export async function sha256hex(s: string): Promise<string> {
  const buf = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(buf)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

function cleanMacs(raw: unknown): string[] {
  if (!Array.isArray(raw)) return [];
  const out: string[] = [];
  for (const m of raw) {
    if (typeof m !== "string") continue;
    const s = m.trim().toLowerCase();
    if (s.length < 11 || s.length > 17) continue;
    if (!/^[0-9a-f:.-]+$/.test(s)) continue;
    if (!out.includes(s)) out.push(s);
    if (out.length >= 8) break;
  }
  return out;
}

function hexRandom(n: number): string {
  const b = new Uint8Array(n);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

function cookieVal(request: Request, name: string): string {
  const c = request.headers.get("Cookie") ?? "";
  for (const part of c.split(";")) {
    const p = part.trim();
    if (p.startsWith(name + "=")) return decodeURIComponent(p.slice(name.length + 1));
  }
  return "";
}

function requestSession(request: Request, url: URL): string {
  const q = url.searchParams.get("session") ?? "";
  if (q) return q;
  const ck = cookieVal(request, "streamaid_session");
  if (ck) return ck;
  const auth = request.headers.get("Authorization") ?? "";
  if (auth.startsWith("Bearer ")) return auth.slice(7);
  return "";
}

function asBuf(u: Uint8Array): ArrayBuffer {
  return u.buffer.slice(u.byteOffset, u.byteOffset + u.byteLength) as ArrayBuffer;
}

function b64ToU8(b64: string): Uint8Array {
  const bin = atob(b64);
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  return out;
}

function safeDownloadName(name: string): string | null {
  const n = name.trim();
  if (!n || n.length > 128 || n.startsWith(".") || n.includes("..")) return null;
  if (/[\\/\0]/.test(n) || [...n].some((c) => c.charCodeAt(0) < 32)) return null;
  return n;
}

function cleanRoot(raw: string): string {
  const s = raw.trim().toLowerCase();
  if (s === "home" || s === "desktop" || s === "documents" || s === "downloads") return s;
  return "inbox";
}

function cleanRel(raw: string): string {
  const parts = raw.split(/[/\\]/).map((s) => s.trim()).filter(Boolean);
  const out: string[] = [];
  for (const p of parts) {
    if (!safeDownloadName(p)) continue;
    out.push(p);
    if (out.length >= 8) break;
  }
  return out.join("/");
}

function corsHeaders(): Record<string, string> {
  return {
    "content-type": "application/json",
    "cache-control": "no-store",
    "access-control-allow-origin": "*",
    "access-control-allow-headers": "Authorization, Content-Type",
    "access-control-allow-methods": "GET, POST, OPTIONS",
  };
}

async function drain(request: Request): Promise<void> {
  try {
    await request.arrayBuffer();
  } catch {
    /* already consumed or closed */
  }
}

async function readJsonObject(
  request: Request,
): Promise<{ ok: true; value: Record<string, unknown> } | { ok: false; error: string }> {
  let raw: unknown;
  try {
    raw = await request.json();
  } catch {
    return { ok: false, error: "invalid JSON body" };
  }
  if (raw === null || typeof raw !== "object" || Array.isArray(raw)) {
    return { ok: false, error: "expected JSON object" };
  }
  return { ok: true, value: raw as Record<string, unknown> };
}

const CHAT_MAX = 2000;
const CHAT_HISTORY = 40;

function clampChat(raw: unknown): string {
  if (typeof raw !== "string") return "";
  const t = raw.trim();
  if (!t) return "";
  let out = "";
  let n = 0;
  for (const ch of t) {
    if (n >= CHAT_MAX) break;
    out += ch;
    n += 1;
  }
  return out;
}

function requiredString(obj: Record<string, unknown>, field: string): string | { error: string } {
  if (!(field in obj)) return { error: `missing ${field}` };
  if (typeof obj[field] !== "string") return { error: `${field} must be a string` };
  const s = (obj[field] as string).trim();
  if (!s) return { error: `missing ${field}` };
  return s;
}

export class StreamRoom extends DurableObject<Env> {
  private lastInit: ArrayBuffer | null = null;
  private lastFrag: ArrayBuffer | null = null;
  private lastSnap: Uint8Array | null = null;
  private lastAnalysis: Analysis | null = null;
  private history: Analysis[] = [];
  private analyzing = false;
  private snapSeq = 0;
  private analyzedSeq = 0;
  private otpHash = "";
  private otpExp = 0;
  private unattendedHash = "";
  private sessions = new Map<string, number>();
  private fails = 0;
  private lockUntil = 0;
  private flags = {
    control: false,
    ai: false,
    audio: false,
    preset: "quality",
    voice: false,
    macs: [] as string[],
  };
  private flagsHydrated = false;
  private controller: string | null = null;
  private display = "";
  private displays: unknown[] = [];
  private fileDl: {
    name: string;
    writer: WritableStreamDefaultWriter<Uint8Array>;
    timer: ReturnType<typeof setTimeout>;
  } | null = null;
  private chatLog: { type: "chat"; text: string; from: string; ts: number }[] = [];

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
    this.ctx.setHibernatableWebSocketEventTimeout(60_000);
  }

  async fetch(request: Request): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname.startsWith("/api/")) {
      return this.handleApi(request, url);
    }
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("Expected WebSocket", { status: 426 });
    }
    const role: ConnRole = url.pathname.endsWith("/publish") ? "publisher" : "viewer";
    if (role === "viewer") {
      const sess = requestSession(request, url);
      if (!(await this.sessionOk(sess))) {
        return Response.json({ error: "unauthorized" }, { status: 401, headers: corsHeaders() });
      }
    }
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server);
    const session = role === "viewer" ? requestSession(request, url) : undefined;
    server.serializeAttachment({ role, session } satisfies Attachment);

    if (role === "publisher") {
      for (const ws of this.ctx.getWebSockets()) {
        if (ws === server) continue;
        const att = ws.deserializeAttachment() as Attachment | null;
        if (att?.role === "publisher") {
          try {
            ws.close(CLOSE_REPLACED, "replaced");
          } catch {
            /* ignore */
          }
        }
      }
      await this.hydrateAuth();
      this.broadcastViewers(this.flagsJson());
    } else {
      await this.hydrateAuth();
      if (this.lastInit) {
        try {
          server.send(this.lastInit);
        } catch {
          /* ignore */
        }
      }
      if (this.lastFrag) {
        try {
          server.send(this.lastFrag);
        } catch {
          /* ignore */
        }
      }
      try {
        server.send(this.flagsJson());
      } catch {
        /* ignore */
      }
      if (this.chatLog.length) {
        try {
          server.send(JSON.stringify({ type: "chat-history", messages: this.chatLog }));
        } catch {
          /* ignore */
        }
      }
    }

    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const att = ws.deserializeAttachment() as Attachment | null;
    if (typeof message === "string") {
      await this.handleJson(ws, att, message);
      return;
    }
    if (att?.role !== "publisher") return;
    const bytes = message instanceof ArrayBuffer ? new Uint8Array(message) : new Uint8Array(message);
    if (bytes.length < 1) return;
    const kind = bytes[0];
    if (kind === TYPE_SNAP) {
      this.lastSnap = bytes.slice(1);
      this.snapSeq += 1;
      // Do not persist the JPEG on the live-video path: storage I/O takes
      // the Durable Object input gate and stalls /publish for seconds.
      if (this.apiKey() && !this.analyzing) {
        void this.ctx.storage.setAlarm(Date.now() + 1500);
      }
      return;
    }
    if (kind === TYPE_INIT) this.lastInit = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    if (kind === TYPE_FRAG) this.lastFrag = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    for (const peer of this.ctx.getWebSockets()) {
      if (peer === ws) continue;
      const p = peer.deserializeAttachment() as Attachment | null;
      if (p?.role !== "viewer") continue;
      if (peer.readyState !== WebSocket.OPEN) continue;
      try {
        peer.send(message);
      } catch {
        /* drop-oldest equivalent: skip a full send buffer */
      }
    }
  }

  private async handleJson(ws: WebSocket, att: Attachment | null, raw: string): Promise<void> {
    let v: { type?: string; hash?: string; exp?: number; control?: boolean; ai?: boolean; audio?: boolean; voice?: boolean; action?: string; x?: number; y?: number; text?: string; key?: string; dy?: number; task?: string };
    try {
      v = JSON.parse(raw) as typeof v;
    } catch {
      return;
    }
    if (att?.role === "publisher") {
      if (v.type === "otp" && typeof v.hash === "string") {
        this.otpHash = v.hash;
        this.otpExp = Number(v.exp) || Date.now() + 300_000;
        const rec = v as { unattended?: string };
        this.unattendedHash =
          typeof rec.unattended === "string" && rec.unattended.length === 64 ? rec.unattended : "";
        this.fails = 0;
        this.lockUntil = 0;
        await this.ctx.storage.put("otp", {
          hash: this.otpHash,
          exp: this.otpExp,
          unattended: this.unattendedHash,
        });
      }
      if (v.type === "flags") {
        this.flags.control = !!v.control;
        this.flags.ai = !!v.ai;
        this.flags.audio = !!v.audio;
        this.flags.voice = !!v.voice;
        const rec = v as { display?: string; displays?: unknown[]; preset?: string; macs?: unknown };
        if (typeof rec.preset === "string" && rec.preset) this.flags.preset = rec.preset;
        this.flagsHydrated = true;
        if (typeof rec.display === "string") this.display = rec.display;
        if (Array.isArray(rec.displays)) this.displays = rec.displays;
        const macs = cleanMacs(rec.macs);
        if (macs.length) this.flags.macs = macs;
        await this.ctx.storage.put("flags", this.flags);
        this.broadcastViewers(this.flagsJson());
      }
      if (v.type === "clipboard") {
        this.broadcastViewers(raw);
      }
      if (v.type === "thumbs") {
        this.broadcastViewers(raw);
      }
      if (v.type === "file") {
        const rec = v as { action?: string; data?: string; error?: string; name?: string };
        const blob =
          rec.action === "blob" ||
          rec.action === "blob-begin" ||
          rec.action === "blob-chunk" ||
          rec.action === "blob-end" ||
          rec.action === "error";
        if (this.fileDl && blob) {
          void this.feedFileDownload(rec);
          return;
        }
        this.broadcastViewers(raw);
      }
      if (v.type === "chat") {
        this.fanChat(raw, "host");
      }
      return;
    }
    if (att?.role !== "viewer") return;
    const session = att.session || "";
    if (!(await this.sessionOk(session))) return;
    if (v.type === "control") {
      if (!this.flags.control) return;
      if (this.controller && this.controller !== session) return;
      this.controller = session;
      this.sendPublisher(JSON.stringify({ ...v, session, type: "control" }));
      return;
    }
    if (v.type === "file") {
      if (!this.flags.control) return;
      this.sendPublisher(JSON.stringify({ ...v, session, type: "file" }));
      return;
    }
    if (v.type === "display") {
      if (!this.flags.control) return;
      this.sendPublisher(JSON.stringify({ ...v, session, type: "display" }));
      return;
    }
    if (v.type === "quality") {
      this.sendPublisher(JSON.stringify({ ...v, session, type: "quality" }));
      return;
    }
    if (v.type === "voice") {
      if (!this.flags.voice) return;
      this.sendPublisher(JSON.stringify({ ...v, session, type: "voice" }));
      return;
    }
    if (v.type === "computer-use") {
      if (!this.flags.ai) return;
      this.sendPublisher(JSON.stringify({ type: "computer-use", task: v.task || "", session }));
    }
    if (v.type === "chat") {
      this.fanChat(raw, "viewer");
    }
  }

  private fanChat(raw: string, from: "host" | "viewer"): void {
    let text = "";
    try {
      const rec = JSON.parse(raw) as { text?: unknown };
      text = clampChat(rec.text);
    } catch {
      return;
    }
    if (!text) return;
    const msg = JSON.stringify({ type: "chat", text, from, ts: Date.now() });
    this.chatLog.push(JSON.parse(msg) as (typeof this.chatLog)[number]);
    if (this.chatLog.length > CHAT_HISTORY) this.chatLog.splice(0, this.chatLog.length - CHAT_HISTORY);
    this.broadcastViewers(msg);
    if (from === "viewer") this.sendPublisher(msg);
  }

  private async feedFileDownload(v: {
    action?: string;
    data?: string;
    error?: string;
  }): Promise<void> {
    const dl = this.fileDl;
    if (!dl) return;
    try {
      if (v.action === "error") {
        clearTimeout(dl.timer);
        this.fileDl = null;
        await dl.writer.abort(v.error || "file");
        return;
      }
      if (v.action === "blob" && typeof v.data === "string") {
        await dl.writer.write(b64ToU8(v.data));
        clearTimeout(dl.timer);
        this.fileDl = null;
        await dl.writer.close();
        return;
      }
      if (v.action === "blob-chunk" && typeof v.data === "string") {
        await dl.writer.write(b64ToU8(v.data));
        return;
      }
      if (v.action === "blob-end") {
        clearTimeout(dl.timer);
        this.fileDl = null;
        await dl.writer.close();
      }
    } catch {
      clearTimeout(dl.timer);
      this.fileDl = null;
    }
  }

  private sendPublisher(msg: string): void {
    for (const peer of this.ctx.getWebSockets()) {
      const p = peer.deserializeAttachment() as Attachment | null;
      if (p?.role !== "publisher" || peer.readyState !== WebSocket.OPEN) continue;
      try {
        peer.send(msg);
      } catch {
        /* ignore */
      }
    }
  }

  private broadcastViewers(msg: string): void {
    for (const peer of this.ctx.getWebSockets()) {
      const p = peer.deserializeAttachment() as Attachment | null;
      if (p?.role !== "viewer" || peer.readyState !== WebSocket.OPEN) continue;
      try {
        peer.send(msg);
      } catch {
        /* skip a full send buffer */
      }
    }
  }

  private async sessionOk(token: string): Promise<boolean> {
    if (!token) return false;
    await this.hydrateAuth();
    const hash = await sha256hex(token);
    const exp = this.sessions.get(hash);
    if (!exp) return false;
    if (exp <= Date.now()) {
      this.sessions.delete(hash);
      return false;
    }
    return true;
  }

  private async hydrateAuth(): Promise<void> {
    if (!this.otpHash) {
      const otp = await this.ctx.storage.get<{ hash: string; exp: number; unattended?: string }>("otp");
      if (otp?.hash) {
        this.otpHash = otp.hash;
        this.otpExp = otp.exp;
        this.unattendedHash = otp.unattended || "";
      }
    }
    if (this.sessions.size === 0) {
      const sess = await this.ctx.storage.get<Record<string, number>>("sessions");
      if (sess) {
        for (const [k, exp] of Object.entries(sess)) this.sessions.set(k, exp);
      }
    }
    if (!this.flagsHydrated) {
      const flags = await this.ctx.storage.get<{
        control: boolean;
        ai: boolean;
        audio?: boolean;
        preset?: string;
        voice?: boolean;
        macs?: string[];
      }>("flags");
      if (flags) {
        this.flags = {
          control: !!flags.control,
          ai: !!flags.ai,
          audio: !!flags.audio,
          preset: flags.preset || "quality",
          voice: !!flags.voice,
          macs: cleanMacs(flags.macs),
        };
      }
      this.flagsHydrated = true;
    }
  }

  private isPublisher(ws: WebSocket): boolean {
    try {
      const att = ws.deserializeAttachment() as Attachment | null;
      return att?.role === "publisher";
    } catch {
      return false;
    }
  }

  private publisherLive(except?: WebSocket): boolean {
    for (const peer of this.ctx.getWebSockets()) {
      if (except && peer === except) continue;
      if (peer.readyState !== WebSocket.OPEN) continue;
      if (this.isPublisher(peer)) return true;
    }
    return false;
  }

  private flagsJson(except?: WebSocket): string {
    return JSON.stringify({
      type: "flags",
      control: this.flags.control,
      ai: this.flags.ai,
      audio: this.flags.audio,
      voice: this.flags.voice,
      preset: this.flags.preset,
      display: this.display,
      displays: this.displays,
      macs: this.flags.macs,
      publisher: this.publisherLive(except),
    });
  }

  private async persistSessions(): Promise<void> {
    const obj: Record<string, number> = {};
    for (const [k, v] of this.sessions) obj[k] = v;
    await this.ctx.storage.put("sessions", obj);
  }

  private dropViewerController(ws: WebSocket): void {
    let att: Attachment | null = null;
    try {
      att = ws.deserializeAttachment() as Attachment | null;
    } catch {
      return;
    }
    if (att?.role === "viewer" && att.session && this.controller === att.session) {
      this.controller = null;
      this.sendPublisher(JSON.stringify({ type: "revoke", session: att.session }));
    }
  }

  async webSocketClose(ws: WebSocket, code: number, reason: string): Promise<void> {
    this.dropViewerController(ws);
    const wasPublisher = this.isPublisher(ws);
    const safe = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1000;
    try {
      ws.close(safe, reason);
    } catch {
      /* ignore */
    }
    if (wasPublisher) {
      await this.hydrateAuth();
      this.broadcastViewers(this.flagsJson(ws));
    }
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    console.error(JSON.stringify({ message: "websocket error", error: String(error) }));
    this.dropViewerController(ws);
    const wasPublisher = this.isPublisher(ws);
    try {
      ws.close(1011, "error");
    } catch {
      /* ignore */
    }
    if (wasPublisher) {
      await this.hydrateAuth();
      this.broadcastViewers(this.flagsJson(ws));
    }
  }

  async alarm(): Promise<void> {
    await this.hydrate();
    await this.runAnalysis();
  }

  private async hydrate(): Promise<void> {
    if (!this.lastSnap) {
      const snap = await this.ctx.storage.get<ArrayBuffer>("snap");
      if (snap) this.lastSnap = new Uint8Array(snap);
    }
    if (!this.lastAnalysis) {
      const last = await this.ctx.storage.get<string>("last");
      if (last) {
        try {
          this.lastAnalysis = JSON.parse(last) as Analysis;
        } catch {
          this.lastAnalysis = null;
        }
      }
    }
    if (this.history.length === 0) {
      const hist = await this.ctx.storage.get<Analysis[]>("history");
      if (Array.isArray(hist)) this.history = hist;
    }
  }

  private async persistSnap(): Promise<void> {
    // Tiny payloads are test stubs; skip storage so vitest isolated SQLite stays clean.
    if (!this.lastSnap || this.lastSnap.length < 128) return;
    try {
      await this.ctx.storage.put("snap", asBuf(this.lastSnap));
    } catch (e) {
      console.error(JSON.stringify({ message: "persist snap failed", error: String(e) }));
    }
  }

  private async persistAnalysis(a: Analysis): Promise<void> {
    try {
      await this.ctx.storage.put("last", JSON.stringify(a));
      if (!a.error && a.summary) {
        await this.ctx.storage.put("history", this.history.slice(0, 30));
      }
    } catch (e) {
      console.error(JSON.stringify({ message: "persist analysis failed", error: String(e) }));
    }
  }

  private apiKey(): string {
    return (this.env.DEEPSEEK_API_KEY ?? "").trim();
  }

  private llmReady(): { ok: true } | { ok: false; error: string } {
    if (!this.apiKey()) {
      return {
        ok: false,
        error: "DEEPSEEK_API_KEY not set. Run: npx wrangler secret put DEEPSEEK_API_KEY",
      };
    }
    if (!this.lastSnap || this.lastSnap.length < 4) {
      return { ok: false, error: "no screenshot yet — origin sends a snapshot every ~8s while publishing" };
    }
    return { ok: true };
  }

  private async handleApi(request: Request, url: URL): Promise<Response> {
    const headers = corsHeaders();
    if (url.pathname === "/api/otp/redeem" && request.method === "POST") {
      return this.redeem(request, headers);
    }
    const sess = requestSession(request, url);
    if (!(await this.sessionOk(sess))) {
      await drain(request);
      return Response.json({ error: "unauthorized" }, { status: 401, headers });
    }
    if (url.pathname === "/api/analysis" && request.method === "GET") {
      return Response.json(
        {
          last: this.lastAnalysis,
          history: this.history,
          llm: this.llmStatus(),
          control: {
            enabled: this.flags.control,
            ai_enabled: this.flags.ai,
            audio: this.flags.audio,
            voice: this.flags.voice,
            display: this.display,
            displays: this.displays,
            macs: this.flags.macs,
            publisher: this.publisherLive(),
          },
        },
        { headers },
      );
    }
    if (url.pathname === "/api/llm-status" && request.method === "GET") {
      return Response.json(this.llmStatus(), { headers });
    }
    if (url.pathname === "/api/files/download" && request.method === "GET") {
      if (!this.flags.control) {
        return Response.json({ error: "remote control disabled" }, { status: 403, headers });
      }
      const name = safeDownloadName(url.searchParams.get("name") ?? "");
      if (!name) {
        return Response.json({ error: "missing name" }, { status: 400, headers });
      }
      if (this.fileDl) {
        return Response.json({ error: "download busy" }, { status: 409, headers });
      }
      const { readable, writable } = new TransformStream<Uint8Array>();
      const writer = writable.getWriter();
      const timer = setTimeout(() => {
        if (this.fileDl && this.fileDl.writer === writer) {
          void writer.abort("timeout");
          this.fileDl = null;
        }
      }, 20_000);
      this.fileDl = { name, writer, timer };
      const root = cleanRoot(url.searchParams.get("root") ?? "");
      const path = cleanRel(url.searchParams.get("path") ?? "");
      this.sendPublisher(JSON.stringify({ type: "file", action: "get", name, root, path, session: sess }));
      const disp = `attachment; filename="${name.replace(/"/g, "")}"`;
      return new Response(readable, {
        headers: {
          "content-type": "application/octet-stream",
          "content-disposition": disp,
          "cache-control": "no-store",
        },
      });
    }
    if (url.pathname === "/api/computer-use" && request.method === "POST") {
      if (!this.flags.ai) {
        await drain(request);
        return Response.json({ error: "ai control disabled" }, { status: 403, headers });
      }
      const parsed = await readJsonObject(request);
      if (!parsed.ok) {
        return Response.json({ error: parsed.error }, { status: 400, headers });
      }
      const task = requiredString(parsed.value, "task");
      if (typeof task !== "string") {
        return Response.json({ error: task.error }, { status: 400, headers });
      }
      this.sendPublisher(JSON.stringify({ type: "computer-use", task, session: sess }));
      return Response.json({ ok: true, accepted: true }, { headers });
    }
    if (url.pathname === "/api/analyze-now" && request.method === "POST") {
      try {
        await request.text();
      } catch {
        /* ignore */
      }
      if (this.apiKey()) await this.hydrate();
      const ready = this.llmReady();
      if (!ready.ok) {
        return Response.json({ error: ready.error }, { status: 503, headers });
      }
      const result = await this.runAnalysis();
      return Response.json(result, { headers });
    }
    if (url.pathname === "/api/ask" && request.method === "POST") {
      const parsed = await readJsonObject(request);
      if (!parsed.ok) {
        return Response.json({ error: parsed.error }, { status: 400, headers });
      }
      const question = requiredString(parsed.value, "question");
      if (typeof question !== "string") {
        return Response.json({ error: question.error }, { status: 400, headers });
      }
      if (this.apiKey()) await this.hydrate();
      const ready = this.llmReady();
      if (!ready.ok) {
        return Response.json({ error: ready.error }, { status: 503, headers });
      }
      try {
        const text = await completeVision(
          this.lastSnap!,
          askPrompt(question),
          this.apiKey(),
          this.env.DEEPSEEK_BASE_URL || DEFAULT_BASE,
          this.env.DEEPSEEK_MODEL || DEFAULT_MODEL,
        );
        const parsed = askFromModel(parseJsonObject(text));
        return Response.json(parsed, { headers });
      } catch (e) {
        const msg = e instanceof Error ? e.message : String(e);
        console.error(JSON.stringify({ message: "llm ask failed", error: msg }));
        return Response.json({ error: msg }, { status: 502, headers });
      }
    }
    return Response.json({ error: "not found" }, { status: 404, headers });
  }

  private async redeem(request: Request, headers: Record<string, string>): Promise<Response> {
    await this.hydrateAuth();
    const parsed = await readJsonObject(request);
    if (!parsed.ok) {
      return Response.json({ error: parsed.error }, { status: 400, headers });
    }
    const pinOrErr = requiredString(parsed.value, "pin");
    if (typeof pinOrErr !== "string") {
      return Response.json({ error: pinOrErr.error }, { status: 400, headers });
    }
    const pin = pinOrErr;
    const now = Date.now();
    if (this.lockUntil && now < this.lockUntil) {
      return Response.json({ error: "rate limited" }, { status: 429, headers });
    }
    const hash = await sha256hex(pin);
    const enc = new TextEncoder();
    const ga = await crypto.subtle.digest("SHA-256", enc.encode(hash));
    const pinOk =
      !!this.otpHash &&
      this.otpExp > now &&
      pin.length === 6 &&
      crypto.subtle.timingSafeEqual(
        ga,
        await crypto.subtle.digest("SHA-256", enc.encode(this.otpHash || "none")),
      );
    const unattendedOk =
      !!this.unattendedHash &&
      crypto.subtle.timingSafeEqual(
        ga,
        await crypto.subtle.digest("SHA-256", enc.encode(this.unattendedHash || "none")),
      );
    if (!pinOk && !unattendedOk) {
      this.fails += 1;
      if (this.fails >= FAIL_LIMIT) {
        this.lockUntil = now + LOCKOUT_MS;
        this.fails = 0;
      }
      return Response.json({ error: "unauthorized" }, { status: 401, headers });
    }
    this.fails = 0;
    const token = hexRandom(32);
    const sh = await sha256hex(token);
    this.sessions.set(sh, now + SESSION_TTL_MS);
    await this.persistSessions();
    const maxAge = Math.floor(SESSION_TTL_MS / 1000);
    headers = {
      ...headers,
      "set-cookie": `streamaid_session=${token}; Path=/; HttpOnly; SameSite=Lax; Max-Age=${maxAge}`,
    };
    return Response.json({ session: token, expires_in_s: maxAge }, { headers });
  }

  private llmStatus() {
    return {
      configured: !!this.apiKey(),
      model: this.env.DEEPSEEK_MODEL || DEFAULT_MODEL,
      has_snapshot: !!(this.lastSnap && this.lastSnap.length >= 4),
      analyzing: this.analyzing,
      last_error: this.lastAnalysis?.error || "",
    };
  }

  private broadcastAnalysis(a: Analysis): void {
    const payload = JSON.stringify({ type: "analysis", data: a });
    for (const peer of this.ctx.getWebSockets()) {
      const p = peer.deserializeAttachment() as Attachment | null;
      if (p?.role !== "viewer" || peer.readyState !== WebSocket.OPEN) continue;
      try {
        peer.send(payload);
      } catch {
        /* ignore */
      }
    }
  }

  private async runAnalysis(): Promise<Analysis> {
    const ready = this.llmReady();
    if (!ready.ok) {
      const a: Analysis = { ts: new Date().toISOString(), summary: "", questions: [], error: ready.error };
      this.lastAnalysis = a;
      return a;
    }
    if (this.analyzing) {
      return this.lastAnalysis ?? { ts: new Date().toISOString(), summary: "", questions: [] };
    }
    this.analyzing = true;
    const seq = this.snapSeq;
    try {
      const text = await completeVision(
        this.lastSnap!,
        ANALYZE_PROMPT,
        this.apiKey(),
        this.env.DEEPSEEK_BASE_URL || DEFAULT_BASE,
        this.env.DEEPSEEK_MODEL || DEFAULT_MODEL,
      );
      const a = analysisFromModel(parseJsonObject(text));
      this.lastAnalysis = a;
      this.history.unshift(a);
      this.history = this.history.slice(0, 30);
      await this.persistAnalysis(a);
      this.broadcastAnalysis(a);
      return a;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      console.error(JSON.stringify({ message: "llm analyze failed", error: msg }));
      const a: Analysis = { ts: new Date().toISOString(), summary: "", questions: [], error: msg };
      this.lastAnalysis = a;
      await this.persistAnalysis(a);
      return a;
    } finally {
      this.analyzing = false;
      this.analyzedSeq = seq;
      if (this.snapSeq > seq && this.apiKey()) {
        await this.ctx.storage.setAlarm(Date.now() + 500);
      }
    }
  }
}
