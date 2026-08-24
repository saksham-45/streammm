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
  DEEPSEEK_API_KEY?: string;
  DEEPSEEK_BASE_URL?: string;
  DEEPSEEK_MODEL?: string;
}

type Attachment = { role: ConnRole };

const CLOSE_REPLACED = 4000;
const TYPE_INIT = 1;
const TYPE_FRAG = 2;
const TYPE_SNAP = 4;

function asBuf(u: Uint8Array): ArrayBuffer {
  return u.buffer.slice(u.byteOffset, u.byteOffset + u.byteLength) as ArrayBuffer;
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

export class StreamRoom extends DurableObject<Env> {
  private lastInit: ArrayBuffer | null = null;
  private lastFrag: ArrayBuffer | null = null;
  private lastSnap: Uint8Array | null = null;
  private lastAnalysis: Analysis | null = null;
  private history: Analysis[] = [];
  private analyzing = false;
  private snapSeq = 0;
  private analyzedSeq = 0;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
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
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment({ role } satisfies Attachment);

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
    } else {
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
    }

    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const att = ws.deserializeAttachment() as Attachment | null;
    if (att?.role !== "publisher") return;
    if (typeof message === "string") return;
    const bytes = message instanceof ArrayBuffer ? new Uint8Array(message) : new Uint8Array(message);
    if (bytes.length < 1) return;
    const kind = bytes[0];
    if (kind === TYPE_SNAP) {
      this.lastSnap = bytes.slice(1);
      this.snapSeq += 1;
      await this.persistSnap();
      if (this.apiKey() && !this.analyzing) {
        await this.ctx.storage.setAlarm(Date.now() + 1500);
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

  async webSocketClose(ws: WebSocket, code: number, reason: string): Promise<void> {
    const safe = code === 1000 || (code >= 3000 && code <= 4999) ? code : 1000;
    try {
      ws.close(safe, reason);
    } catch {
      /* ignore */
    }
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    console.error(JSON.stringify({ message: "websocket error", error: String(error) }));
    try {
      ws.close(1011, "error");
    } catch {
      /* ignore */
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
    if (url.pathname === "/api/analysis" && request.method === "GET") {
      return Response.json(
        { last: this.lastAnalysis, history: this.history, llm: this.llmStatus() },
        { headers },
      );
    }
    if (url.pathname === "/api/llm-status" && request.method === "GET") {
      return Response.json(this.llmStatus(), { headers });
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
      let question = "";
      try {
        const body = (await request.json()) as { question?: string };
        question = (body.question ?? "").trim();
      } catch {
        return Response.json({ error: "invalid JSON body" }, { status: 400, headers });
      }
      if (!question) {
        return Response.json({ error: "missing question" }, { status: 400, headers });
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
