import { PLAYER_HTML } from "./player";
import { StreamRoom, type Env } from "./room";

export { StreamRoom };

async function tokenOk(request: Request, env: Env): Promise<boolean> {
  const expected = env.STREAM_TOKEN ?? "";
  if (!expected) return true;
  const url = new URL(request.url);
  let given = url.searchParams.get("token") ?? "";
  const auth = request.headers.get("Authorization") ?? "";
  if (auth.startsWith("Bearer ")) given = auth.slice(7);
  const enc = new TextEncoder();
  const [ga, ea] = await Promise.all([
    crypto.subtle.digest("SHA-256", enc.encode(given)),
    crypto.subtle.digest("SHA-256", enc.encode(expected)),
  ]);
  return crypto.subtle.timingSafeEqual(ga, ea);
}

function cors(): HeadersInit {
  return {
    "Access-Control-Allow-Origin": "*",
    "Access-Control-Allow-Headers": "Authorization, Content-Type",
    "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
  };
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    if (request.method === "OPTIONS") {
      return new Response(null, { headers: cors() });
    }
    const url = new URL(request.url);
    if (url.pathname === "/" || url.pathname === "/view") {
      return new Response(PLAYER_HTML, {
        headers: { "content-type": "text/html; charset=utf-8", ...cors() },
      });
    }
    if (url.pathname === "/health") {
      return Response.json({ ok: true }, { headers: cors() });
    }
    if (!(await tokenOk(request, env))) {
      console.log(JSON.stringify({ message: "unauthorized", path: url.pathname }));
      return Response.json({ error: "unauthorized" }, { status: 401, headers: cors() });
    }
    if (
      url.pathname === "/publish" ||
      url.pathname === "/watch" ||
      url.pathname === "/stream.ws"
    ) {
      const stub = env.STREAM_ROOM.getByName("default");
      return stub.fetch(request);
    }
    return new Response("not found", { status: 404 });
  },
} satisfies ExportedHandler<Env>;
