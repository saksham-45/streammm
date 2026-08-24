import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";

async function openWs(path: string, token = "secret"): Promise<WebSocket> {
  const q = `${path}?token=${encodeURIComponent(token)}`;
  const res = await SELF.fetch(`https://example.com${q}`, {
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
    const pub = await openWs("/publish");
    const view = await openWs("/watch");
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
    const pub = await openWs("/publish");
    pub.send(new Uint8Array([1, 1, 1]));
    pub.send(new Uint8Array([2, 2, 2]));
    await new Promise((r) => setTimeout(r, 50));
    const view = await openWs("/watch");
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
    const a = await openWs("/publish");
    const closed = new Promise<number>((resolve) => {
      a.addEventListener("close", (ev) => resolve((ev as CloseEvent).code));
    });
    const b = await openWs("/publish");
    const code = await Promise.race([
      closed,
      new Promise<number>((_, rej) => setTimeout(() => rej(new Error("no replace")), 3000)),
    ]);
    expect(code).toBe(4000);
    b.close();
  });
});
