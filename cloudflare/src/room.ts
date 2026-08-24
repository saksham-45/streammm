import { DurableObject } from "cloudflare:workers";

export type ConnRole = "publisher" | "viewer";

export interface Env {
  STREAM_ROOM: DurableObjectNamespace;
  STREAM_TOKEN?: string;
}

type Attachment = { role: ConnRole };

const CLOSE_REPLACED = 4000;

export class StreamRoom extends DurableObject<Env> {
  private lastInit: ArrayBuffer | null = null;
  private lastFrag: ArrayBuffer | null = null;

  constructor(ctx: DurableObjectState, env: Env) {
    super(ctx, env);
    this.ctx.setWebSocketAutoResponse(new WebSocketRequestResponsePair("ping", "pong"));
  }

  async fetch(request: Request): Promise<Response> {
    if (request.headers.get("Upgrade") !== "websocket") {
      return new Response("Expected WebSocket", { status: 426 });
    }
    const url = new URL(request.url);
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
    if (kind === 1) this.lastInit = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
    if (kind === 2) this.lastFrag = bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength);
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
    ws.close(code, reason);
  }

  async webSocketError(ws: WebSocket, error: unknown): Promise<void> {
    console.error(JSON.stringify({ message: "websocket error", error: String(error) }));
    try {
      ws.close(1011, "error");
    } catch {
      /* ignore */
    }
  }
}
