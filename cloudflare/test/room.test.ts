import { SELF } from "cloudflare:test";
import { describe, expect, it } from "vitest";
import { isPublicEdge, tokenOk } from "../src/index";
import { sha256hex, type Env } from "../src/room";

async function openPublish(room?: string): Promise<WebSocket> {
  const u = new URL("https://example.com/publish");
  u.searchParams.set("token", "secret");
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

async function installPin(pub: WebSocket, pin: string, exp = Date.now() + 300_000): Promise<void> {
  const hash = await sha256hex(pin);
  pub.send(JSON.stringify({ type: "otp", hash, exp }));
  await new Promise((r) => setTimeout(r, 30));
}

async function redeem(pin: string, room?: string): Promise<string> {
  const u = new URL("https://example.com/api/otp/redeem");
  if (room) u.searchParams.set("room", room);
  const res = await SELF.fetch(u.toString(), {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ pin }),
  });
  expect(res.status).toBe(200);
  const body = await res.json<{ session: string }>();
  expect(body.session).toBeTruthy();
  return body.session;
}

async function openWatch(session: string, room?: string): Promise<WebSocket> {
  const u = new URL("https://example.com/watch");
  u.searchParams.set("session", session);
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

async function viewerSession(room: string, pin = "123456"): Promise<{ pub: WebSocket; session: string }> {
  const pub = await openPublish(room);
  await installPin(pub, pin);
  const session = await redeem(pin, room);
  return { pub, session };
}

describe("StreamRoom", () => {
  it("health is public", async () => {
    const res = await SELF.fetch("https://example.com/health");
    expect(res.status).toBe(200);
  });

  it("EDGE_PUBLIC=off means the public edge is disabled", () => {
    expect(isPublicEdge({ EDGE_PUBLIC: "off" } as Env)).toBe(false);
    expect(isPublicEdge({ EDGE_PUBLIC: "on" } as Env)).toBe(true);
    expect(isPublicEdge({} as Env)).toBe(false);
  });

  it("refuses publish when STREAM_TOKEN is unset", async () => {
    const req = new Request("https://example.com/publish");
    expect(await tokenOk(req, { STREAM_TOKEN: "" } as Env)).toBe(false);
    expect(await tokenOk(req, {} as Env)).toBe(false);
  });

  it("rejects missing/wrong publisher token with 401", async () => {
    const missing = await SELF.fetch("https://example.com/publish", {
      headers: { Upgrade: "websocket" },
    });
    expect(missing.status).toBe(401);
    const wrong = await SELF.fetch("https://example.com/publish?token=nope", {
      headers: { Upgrade: "websocket" },
    });
    expect(wrong.status).toBe(401);
  });

  it("rejects watch without PIN session even with STREAM_TOKEN", async () => {
    const missing = await SELF.fetch("https://example.com/watch", {
      headers: { Upgrade: "websocket" },
    });
    expect(missing.status).toBe(401);
    const withToken = await SELF.fetch("https://example.com/watch?token=secret", {
      headers: { Upgrade: "websocket" },
    });
    expect(withToken.status).toBe(401);
    const api = await SELF.fetch("https://example.com/api/analysis?token=secret");
    expect(api.status).toBe(401);
  });

  it("publisher binary reaches a viewer after PIN redeem (no STREAM_TOKEN on watch URL)", async () => {
    const { pub, session } = await viewerSession("fanout");
    const view = await openWatch(session, "fanout");
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
    const { pub, session } = await viewerSession("late");
    pub.send(new Uint8Array([1, 1, 1]));
    pub.send(new Uint8Array([2, 2, 2]));
    await new Promise((r) => setTimeout(r, 50));
    const view = await openWatch(session, "late");
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
    const a = await openPublish("replace");
    const closed = new Promise<number>((resolve) => {
      a.addEventListener("close", (ev) => resolve((ev as CloseEvent).code));
    });
    const b = await openPublish("replace");
    const code = await Promise.race([
      closed,
      new Promise<number>((_, rej) => setTimeout(() => rej(new Error("no replace")), 3000)),
    ]);
    expect(code).toBe(4000);
    b.close();
  });

  it("exposes analysis APIs with redeemed session and 503 without DeepSeek key", async () => {
    const { session } = await viewerSession("llm-status");
    const res = await SELF.fetch(`https://example.com/api/analysis?session=${session}&room=llm-status`);
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

    const ask = await SELF.fetch(`https://example.com/api/ask?session=${session}&room=llm-status`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: "what is on screen?" }),
    });
    expect(ask.status).toBe(503);
    const askBody = await ask.json<{ error: string }>();
    expect(askBody.error).toMatch(/DEEPSEEK_API_KEY/);

    const now = await SELF.fetch(`https://example.com/api/analyze-now?session=${session}&room=llm-status`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{}",
    });
    expect(now.status).toBe(503);
    await now.text();
  });

  it("does not fan TYPE_SNAP screenshots out to viewers", async () => {
    const { pub, session } = await viewerSession("snap");
    const view = await openWatch(session, "snap");
    let viewerGot = false;
    view.addEventListener("message", (ev) => {
      // JSON flags are fanned to watchers; JPEG snapshots must not be.
      if (typeof ev.data === "string") return;
      viewerGot = true;
    });
    pub.send(new Uint8Array([4, 0xff, 0xd8, 0xff, 0xd9, 1, 2, 3, 4, 5]));
    await new Promise((r) => setTimeout(r, 250));
    expect(viewerGot).toBe(false);

    const res = await SELF.fetch(`https://example.com/api/analysis?session=${session}&room=snap`);
    const body = await res.json<{ llm: { has_snapshot: boolean; configured: boolean } }>();
    expect(body.llm.has_snapshot).toBe(true);
    expect(body.llm.configured).toBe(false);
    pub.close();
    view.close();
  });

  it("rejects empty ask questions with 400", async () => {
    const { session } = await viewerSession("ask-empty");
    const empty = await SELF.fetch(`https://example.com/api/ask?session=${session}&room=ask-empty`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: "   " }),
    });
    expect(empty.status).toBe(400);
    const body = await empty.json<{ error: string }>();
    expect(body.error).toMatch(/question/);
  });

  it("forwards authorized viewer control JSON to publisher", async () => {
    const { pub, session } = await viewerSession("ctl");
    pub.send(JSON.stringify({ type: "flags", control: true, ai: false }));
    await new Promise((r) => setTimeout(r, 30));
    const view = await openWatch(session, "ctl");
    const got = new Promise<string>((resolve) => {
      pub.addEventListener("message", (ev) => {
        if (typeof ev.data === "string") resolve(ev.data);
      });
    });
    view.send(JSON.stringify({ type: "control", action: "click", x: 0.5, y: 0.25 }));
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no control")), 3000)),
    ]);
    const msg = JSON.parse(raw) as { type: string; action: string; x: number; y: number };
    expect(msg.type).toBe("control");
    expect(msg.action).toBe("click");
    expect(msg.x).toBe(0.5);
    expect(msg.y).toBe(0.25);
    pub.close();
    view.close();
  });

  it("forwards right-click, drag, paste, and fans host clipboard to watchers", async () => {
    const { pub, session } = await viewerSession("ctl-human");
    pub.send(JSON.stringify({ type: "flags", control: true, ai: false }));
    await new Promise((r) => setTimeout(r, 30));
    const view = await openWatch(session, "ctl-human");
    const got = new Promise<string>((resolve) => {
      pub.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("right")) resolve(ev.data);
      });
    });
    view.send(
      JSON.stringify({
        type: "control",
        action: "click",
        x: 0.2,
        y: 0.3,
        button: "right",
        modifiers: ["Shift"],
      }),
    );
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no right-click")), 3000)),
    ]);
    const msg = JSON.parse(raw) as {
      type: string;
      action: string;
      button?: string;
      modifiers?: string[];
    };
    expect(msg.action).toBe("click");
    expect(msg.button).toBe("right");
    expect(msg.modifiers).toContain("Shift");

    const clip = new Promise<string>((resolve) => {
      view.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("clipboard")) resolve(ev.data);
      });
    });
    pub.send(JSON.stringify({ type: "clipboard", text: "host-copy" }));
    const clipRaw = await Promise.race([
      clip,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no clipboard fan-out")), 3000)),
    ]);
    const clipMsg = JSON.parse(clipRaw) as { type: string; text: string };
    expect(clipMsg.type).toBe("clipboard");
    expect(clipMsg.text).toBe("host-copy");

    const thumbs = new Promise<string>((resolve) => {
      view.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("thumbs")) resolve(ev.data);
      });
    });
    pub.send(JSON.stringify({ type: "thumbs", items: [{ id: "3:", data: "qq" }] }));
    const thumbsRaw = await Promise.race([
      thumbs,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no thumbs fan-out")), 3000)),
    ]);
    const thumbsMsg = JSON.parse(thumbsRaw) as { type: string; items: { id: string }[] };
    expect(thumbsMsg.type).toBe("thumbs");
    expect(thumbsMsg.items[0].id).toBe("3:");
    pub.close();
    view.close();
  });

  it("forwards viewer file put to publisher and fans origin file list to watchers", async () => {
    const { pub, session } = await viewerSession("files");
    pub.send(JSON.stringify({ type: "flags", control: true, ai: false }));
    await new Promise((r) => setTimeout(r, 30));
    const view = await openWatch(session, "files");
    const got = new Promise<string>((resolve) => {
      pub.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("\"file\"")) resolve(ev.data);
      });
    });
    view.send(
      JSON.stringify({
        type: "file",
        action: "put",
        name: "note.txt",
        data: "aGk=",
      }),
    );
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no file put")), 3000)),
    ]);
    const msg = JSON.parse(raw) as { type: string; action: string; name?: string };
    expect(msg.type).toBe("file");
    expect(msg.action).toBe("put");
    expect(msg.name).toBe("note.txt");

    const list = new Promise<string>((resolve) => {
      view.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("file") && ev.data.includes("list")) {
          resolve(ev.data);
        }
      });
    });
    pub.send(JSON.stringify({ type: "file", action: "list", files: [{ name: "note.txt", size: 2 }] }));
    const listRaw = await Promise.race([
      list,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no file list fan-out")), 3000)),
    ]);
    const listMsg = JSON.parse(listRaw) as { type: string; action: string; files: { name: string }[] };
    expect(listMsg.action).toBe("list");
    expect(listMsg.files[0].name).toBe("note.txt");
    pub.close();
    view.close();
  });

  it("forwards display switch and fans display list on flags", async () => {
    const { pub, session } = await viewerSession("disp");
    const view = await openWatch(session, "disp");
    const flagged = new Promise<string>((resolve) => {
      view.addEventListener("message", (ev) => {
        if (typeof ev.data !== "string") return;
        try {
          const m = JSON.parse(ev.data) as { type?: string; displays?: unknown[] };
          if (m.type === "flags" && Array.isArray(m.displays) && m.displays.length >= 2) resolve(ev.data);
        } catch {
          /* ignore */
        }
      });
    });
    pub.send(JSON.stringify({
      type: "flags",
      control: true,
      ai: false,
      display: "3:",
      displays: [{ id: "3:", name: "Display 1 (main)", main: true }, { id: "4:", name: "Display 2" }],
    }));
    const flagRaw = await Promise.race([
      flagged,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no display flags")), 3000)),
    ]);
    const flagMsg = JSON.parse(flagRaw) as { displays?: { id: string }[] };
    expect(flagMsg.displays && flagMsg.displays.length).toBeGreaterThanOrEqual(2);

    const got = new Promise<string>((resolve) => {
      pub.addEventListener("message", (ev) => {
        if (typeof ev.data === "string" && ev.data.includes("display")) resolve(ev.data);
      });
    });
    view.send(JSON.stringify({ type: "display", id: "4:" }));
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no display switch")), 3000)),
    ]);
    const msg = JSON.parse(raw) as { type: string; id?: string };
    expect(msg.type).toBe("display");
    expect(msg.id).toBe("4:");
    pub.close();
    view.close();
  });

  it("does not forward control when host disabled", async () => {
    const { pub, session } = await viewerSession("ctl-off");
    pub.send(JSON.stringify({ type: "flags", control: false, ai: false }));
    await new Promise((r) => setTimeout(r, 30));
    const view = await openWatch(session, "ctl-off");
    let got = false;
    pub.addEventListener("message", () => {
      got = true;
    });
    view.send(JSON.stringify({ type: "control", action: "click", x: 0.1, y: 0.1 }));
    await new Promise((r) => setTimeout(r, 200));
    expect(got).toBe(false);
    pub.close();
    view.close();
  });

  it("fans host flags out to watchers so AI chrome can appear", async () => {
    const { pub, session } = await viewerSession("flags-fan");
    const view = await openWatch(session, "flags-fan");
    const got = new Promise<string>((resolve) => {
      view.addEventListener("message", (ev) => {
        if (typeof ev.data !== "string") return;
        try {
          const m = JSON.parse(ev.data) as { type?: string; ai?: boolean; control?: boolean };
          if (m.type === "flags" && m.ai === true && m.control === true) resolve(ev.data);
        } catch {
          /* ignore binary */
        }
      });
    });
    pub.send(JSON.stringify({ type: "flags", control: true, ai: true }));
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no flags fan-out")), 3000)),
    ]);
    const msg = JSON.parse(raw) as { type: string; control: boolean; ai: boolean };
    expect(msg.type).toBe("flags");
    expect(msg.control).toBe(true);
    expect(msg.ai).toBe(true);
    pub.close();
    view.close();
  });

  it("computer-use is 403 when AI disabled and forwards when enabled", async () => {
    const { pub, session } = await viewerSession("ai");
    const off = await SELF.fetch(`https://example.com/api/computer-use?session=${session}&room=ai`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ task: "click then type" }),
    });
    expect(off.status).toBe(403);
    const offBody = await off.json<{ error: string }>();
    expect(offBody.error).toBeTruthy();

    pub.send(JSON.stringify({ type: "flags", control: true, ai: true }));
    await new Promise((r) => setTimeout(r, 30));
    const got = new Promise<string>((resolve) => {
      pub.addEventListener("message", (ev) => {
        if (typeof ev.data === "string") resolve(ev.data);
      });
    });
    const on = await SELF.fetch(`https://example.com/api/computer-use?session=${session}&room=ai`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ task: "click then type" }),
    });
    expect(on.status).toBe(200);
    const raw = await Promise.race([
      got,
      new Promise<string>((_, rej) => setTimeout(() => rej(new Error("no ai")), 3000)),
    ]);
    const msg = JSON.parse(raw) as { type: string; task: string };
    expect(msg.type).toBe("computer-use");
    expect(msg.task).toBe("click then type");
    pub.close();
  });

  it("redeemed PIN session lasts a day", async () => {
    const pub = await openPublish("day-session");
    await installPin(pub, "123456");
    const u = new URL("https://example.com/api/otp/redeem");
    u.searchParams.set("room", "day-session");
    const res = await SELF.fetch(u.toString(), {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pin: "123456" }),
    });
    expect(res.status).toBe(200);
    const body = await res.json<{ session: string; expires_in_s: number }>();
    expect(body.expires_in_s).toBe(86400);
    const cookie = res.headers.get("set-cookie") ?? "";
    expect(cookie).toMatch(/Max-Age=86400/);
    pub.close();
  });

  it("wrong PIN is 401; STREAM_TOKEN still required to publish", async () => {
    const pub = await openPublish("badpin");
    await installPin(pub, "654321");
    const res = await SELF.fetch("https://example.com/api/otp/redeem?room=badpin", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pin: "000000" }),
    });
    expect(res.status).toBe(401);
    const body = await res.json<{ error: string }>();
    expect(body.error).toBeTruthy();
    pub.close();
  });

  async function jsonError(res: Response, status: number): Promise<{ error: string }> {
    expect(res.status).toBe(status);
    expect(res.status).toBeLessThan(500);
    const body = await res.json<{ error: string }>();
    expect(body.error).toBeTruthy();
    return body;
  }

  it("redeem rejects malformed, empty, and non-string PIN with 400 JSON", async () => {
    const pub = await openPublish("redeem-400");
    await installPin(pub, "123456");
    const base = "https://example.com/api/otp/redeem?room=redeem-400";

    const malformed = await SELF.fetch(base, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: "{",
    });
    await jsonError(malformed, 400);

    for (const body of [
      JSON.stringify({}),
      JSON.stringify({ pin: "" }),
      JSON.stringify({ pin: "   " }),
      JSON.stringify({ pin: 123456 }),
      JSON.stringify({ pin: true }),
      JSON.stringify({ pin: null }),
    ]) {
      const res = await SELF.fetch(base, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });
      await jsonError(res, 400);
    }

    const ok = await SELF.fetch(base, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pin: "123456" }),
    });
    expect(ok.status).toBe(200);
    const okBody = await ok.json<{ session: string }>();
    expect(okBody.session).toBeTruthy();
    const cookie = ok.headers.get("set-cookie") ?? "";
    expect(cookie).toContain("streamaid_session=");
    pub.close();
  });

  it("redeem rate-limits after FAIL_LIMIT wrong tries with 429 JSON", async () => {
    const pub = await openPublish("redeem-429");
    await installPin(pub, "654321");
    const base = "https://example.com/api/otp/redeem?room=redeem-429";
    for (let i = 0; i < 5; i++) {
      const res = await SELF.fetch(base, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ pin: "000000" }),
      });
      await jsonError(res, 401);
    }
    const limited = await SELF.fetch(base, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ pin: "000000" }),
    });
    await jsonError(limited, 429);
    pub.close();
  });

  it("computer-use and ask reject missing session, bad JSON, and missing fields", async () => {
    const { pub, session } = await viewerSession("api-400");
    pub.send(JSON.stringify({ type: "flags", control: true, ai: true }));
    await new Promise((r) => setTimeout(r, 30));
    const cu = (path: string, body?: string) =>
      SELF.fetch(`https://example.com${path}?session=${session}&room=api-400`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body,
      });

    const cuNoAuth = await SELF.fetch("https://example.com/api/computer-use?room=api-400", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ task: "click" }),
    });
    await jsonError(cuNoAuth, 401);

    const askNoAuth = await SELF.fetch("https://example.com/api/ask?room=api-400", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ question: "what" }),
    });
    await jsonError(askNoAuth, 401);

    await jsonError(await cu("/api/computer-use", "{"), 400);
    await jsonError(await cu("/api/computer-use", JSON.stringify({})), 400);
    await jsonError(await cu("/api/computer-use", JSON.stringify({ task: "" })), 400);
    await jsonError(await cu("/api/computer-use", JSON.stringify({ task: 1 })), 400);

    await jsonError(await cu("/api/ask", "{"), 400);
    await jsonError(await cu("/api/ask", JSON.stringify({})), 400);
    await jsonError(await cu("/api/ask", JSON.stringify({ question: 1 })), 400);

    pub.close();
  });
});
