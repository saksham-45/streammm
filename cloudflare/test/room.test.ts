import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

async function openWs(path: string, token = "secret", room?: string): Promise<WebSocket> {
  const u = new URL(`https://example.com${path}`);
  u.searchParams.set("token", token);
  if (room) u.searchParams.set("room", room);
  const res = await SELF.fetch(u.toString(), {
    headers: { Upgrade: "websocket" },
  });
  expect(res.status).toBe(101);
  const ws = res.webSocket;
  expect(ws).toBeTruthy();
  ws!.accept();
  return ws!;
}

describe("StreamRoom", () => {
  it("rejects missing/wrong token with 401", async () => {
    const missing = await SELF.fetch("https://example.com/watch", {
      headers: { Upgrade: "websocket" },
    });
    expect(missing.status).toBe(401);
    const wrong = await SELF.fetch("https://example.com/watch?token=nope", {
      headers: { Upgrade: "websocket" },
    });
    expect(wrong.status).toBe(401);
  });

  it("publisher binary reaches a viewer", async () => {
    const pub = await openWs("/publish", "secret", "fanout");
    const view = await openWs("/watch", "secret", "fanout");
    const got = new Promise<ArrayBuffer>((resolve) => {
      view.addEventListener("message", (ev) => {
        const data = ev.data;
        if (data instanceof ArrayBuffer) resolve(data);
        else if (typeof data === "object" && data && "arrayBuffer" in (data as Blob)) {
          (data as Blob).arrayBuffer().then(resolve);
        }
      });
    });
    const payload = new Uint8Array([2, 9, 9, 9]);
    pub.send(payload);
    const buf = new Uint8Array(await Promise.race([
      got,
      new Promise<ArrayBuffer>((_, rej) => setTimeout(() => rej(new Error("timeout")), 3000)),
    ]));
    expect(Array.from(buf)).toEqual([2, 9, 9, 9]);
    pub.close();
    view.close();
  });

  it("late viewer receives cached init then latest fragment", async () => {
    const pub = await openWs("/publish", "secret", "late");
    pub.send(new Uint8Array([1, 1, 1]));
    pub.send(new Uint8Array([2, 2, 2]));
    await new Promise((r) => setTimeout(r, 50));
    const view = await openWs("/watch", "secret", "late");
    const msgs: number[][] = [];
    await new Promise<void>((resolve, reject) => {
      const t = setTimeout(() => reject(new Error("late join timeout")), 3000);
      view.addEventListener("message", (ev) => {
        const raw = ev.data;
        const deliver = (ab: ArrayBuffer) => {
          msgs.push(Array.from(new Uint8Array(ab)));
          if (msgs.length >= 2) {
            clearTimeout(t);
            resolve();
          }
        };
        if (raw instanceof ArrayBuffer) deliver(raw);
        else if (raw instanceof Blob) raw.arrayBuffer().then(deliver);
      });
    });
    expect(msgs[0][0]).toBe(1);
    expect(msgs[1][0]).toBe(2);
    pub.close();
    view.close();
  });

  it("second publisher replaces the first", async () => {
    const a = await openWs("/publish", "secret", "replace");
    const closed = new Promise<number>((resolve) => {
      a.addEventListener("close", (ev) => resolve((ev as CloseEvent).code));
    });
    const b = await openWs("/publish", "secret", "replace");
    const code = await Promise.race([
      closed,
      new Promise<number>((_, rej) => setTimeout(() => rej(new Error("no replace")), 3000)),
    ]);
    expect(code).toBe(4000);
    b.close();
  });

  it("exposes analysis APIs without a DeepSeek key as 503 / unconfigured", async () => {
    const missing = await SELF.fetch("https://example.com/api/analysis");
    expect(missing.status).toBe(401);
    await missing.text();

    const res = await SELF.fetch("https://example.com/api/analysis?token=secret&room=llm-status");
    expect(res.status).toBe(200);
    const body = await res.json<{
      last: unknown;
      history: unknown[];
      llm: { configured: boolean; has_snapshot: boolean; model: string };
    }>();
    expect(body.last).toBeNull();
    expect(body.history).toEqual([]);
    expect(body.llm.configured).toBe(false);
    expect(body.llm.has_snapshot).toBe(false);
    expect(body.llm.model).toContain("deepseek");

    const ask = await SELF.fetch("https://example.com/api/ask?token=secret&room=llm-status", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: "what is on screen?" }),
    });
    expect(ask.status).toBe(503);
    const askBody = await ask.json<{ error: string }>();
    expect(askBody.error).toMatch(/DEEPSEEK_API_KEY/);

    const now = await SELF.fetch("https://example.com/api/analyze-now?token=secret&room=llm-status", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    });
    expect(now.status).toBe(503);
    await now.text();
  });

  it("does not fan TYPE_SNAP screenshots out to viewers", async () => {
    const pub = await openWs("/publish", "secret", "snap");
    const view = await openWs("/watch", "secret", "snap");
    let viewerGot = false;
    view.addEventListener("message", () => {
      viewerGot = true;
    });
    pub.send(new Uint8Array([4, 0xff, 0xd8, 0xff, 0xd9, 1, 2, 3, 4, 5]));
    await new Promise((r) => setTimeout(r, 250));
    expect(viewerGot).toBe(false);

    const res = await SELF.fetch("https://example.com/api/analysis?token=secret&room=snap");
    const body = await res.json<{ llm: { has_snapshot: boolean; configured: boolean } }>();
    expect(body.llm.has_snapshot).toBe(true);
    expect(body.llm.configured).toBe(false);
    pub.close();
    view.close();
  });

  it("rejects empty ask questions with 400", async () => {
    const empty = await SELF.fetch("https://example.com/api/ask?token=secret&room=ask-empty", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: "   " }),
    });
    expect(empty.status).toBe(400);
    const body = await empty.json<{ error: string }>();
    expect(body.error).toMatch(/question/);
  });
});
