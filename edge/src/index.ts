// Edge Worker: routing, auth, container lifecycle and binding relays. Product logic lives in the
// Rust containers.
//
//   /api/*      → aiopsd (GET is public; writes need `Authorization: Bearer <ADMIN_TOKEN>`)
//   /runner/*   → runner job API (admin only; for measurements and debugging)
//   POST /admin/restart-aiopsd → restart the control plane with the current config (admin only)
//   cron        → wakes aiopsd while any run is unfinished
//
// Containers reach D1, R2 and the runner only through the outbound handlers below.
import { Container, ContainerProxy, getContainer } from "@cloudflare/containers";

export { ContainerProxy };

/** Generated bindings plus secrets set with `wrangler secret put`. */
type Bindings = Cloudflare.Env & {
  ADMIN_TOKEN: string;
  META_API_KEY?: string;
};

const TERMINAL = "('COMPLETED','EXHAUSTED','FAILED','CANCELLED')";

/** Isolated experiment runner: no internet access, no bindings, no secrets, no outbound handlers. */
export class RunnerContainer extends Container<Bindings> {
  defaultPort = 8080;
  sleepAfter = "20m";
  enableInternet = false;
}

/** Control plane. Internet access is on for the Meta API only; everything else goes through the relays. */
export class AiopsdContainer extends Container<Bindings> {
  defaultPort = 8080;
  sleepAfter = "15m";
  enableInternet = true;

  constructor(ctx: DurableObjectState<{}>, env: Bindings) {
    super(ctx, env);
    this.envVars = {
      AIOPS_DB: "d1:http://d1.internal",
      AIOPS_ARTIFACTS: "r2:http://r2.internal",
      AIOPS_RUNNER_URL: "http://runner.internal",
      META_API_KEY: env.META_API_KEY ?? "",
      META_MODEL: env.META_MODEL ?? "",
      META_BASE_URL: env.META_BASE_URL ?? "https://api.meta.ai/v1",
    };
  }
}

AiopsdContainer.outboundByHost = {
  "runner.internal": (request, env) => getContainer(env.RUNNER, "runner-0").fetch(request),
  "d1.internal": (request, env) => d1Relay(request, env.DB),
  "r2.internal": (request, env) => r2Relay(request, env.ARTIFACTS),
};

const aiopsd = (env: Bindings) => getContainer(env.AIOPSD, "aiopsd-0");

export default {
  async fetch(request, env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/healthz") {
      return Response.json({ ok: true });
    }

    if (url.pathname.startsWith("/api/")) {
      const readOnly = request.method === "GET" || request.method === "HEAD";
      if (!readOnly && !(await authorized(request, env))) {
        return Response.json({ error: "unauthorized" }, { status: 401 });
      }
      return aiopsd(env).fetch(request);
    }

    // A container rollout swaps the image but can keep the environment the container was first
    // started with. Stopping aiopsd (SIGTERM: it finishes its tick and hands over its lease) makes
    // the next request start it from this version, with this version's vars and secrets.
    if (url.pathname === "/admin/restart-aiopsd" && request.method === "POST") {
      if (!(await authorized(request, env))) {
        return Response.json({ error: "unauthorized" }, { status: 401 });
      }
      try {
        await aiopsd(env).stop();
        return Response.json({ stopped: true });
      } catch (err) {
        return Response.json({ stopped: false, error: String(err) }, { status: 500 });
      }
    }

    if (url.pathname === "/runner" || url.pathname.startsWith("/runner/")) {
      if (!(await authorized(request, env))) {
        return Response.json({ error: "unauthorized" }, { status: 401 });
      }
      const target = new URL(url.pathname.slice("/runner".length) || "/", "http://runner");
      target.search = url.search;
      return getContainer(env.RUNNER, "runner-0").fetch(new Request(target, request));
    }

    return Response.json({ error: "not_found" }, { status: 404 });
  },

  async scheduled(_controller, env, ctx): Promise<void> {
    try {
      const active = await env.DB.prepare(`SELECT COUNT(*) AS n FROM runs WHERE state NOT IN ${TERMINAL}`).first<{ n: number }>();
      if (active && active.n > 0) {
        ctx.waitUntil(aiopsd(env).fetch("http://aiopsd/internal/tick", { method: "POST" }));
      }
    } catch (err) {
      console.error("cron tick skipped", err); // e.g. migrations not applied yet
    }
  },
} satisfies ExportedHandler<Bindings>;

async function authorized(request: Request, env: Bindings): Promise<boolean> {
  const token = env.ADMIN_TOKEN ?? "";
  if (token.length < 16) {
    return false; // Refuse to run with a missing or trivially short token.
  }
  const enc = new TextEncoder();
  const given = enc.encode(request.headers.get("authorization") ?? "");
  const expected = enc.encode(`Bearer ${token}`);
  return given.byteLength === expected.byteLength && crypto.subtle.timingSafeEqual(given, expected);
}

type SqlStatement = { sql: string; params?: unknown[] };

/** `POST /query {sql, params}` and `POST /batch {statements}` (one transaction) → D1. */
async function d1Relay(request: Request, db: D1Database): Promise<Response> {
  if (request.method !== "POST") {
    return Response.json({ error: "method not allowed" }, { status: 405 });
  }
  const path = new URL(request.url).pathname;
  try {
    if (path === "/query") {
      const s = await request.json<SqlStatement>();
      const r = await db.prepare(s.sql).bind(...(s.params ?? [])).run();
      return Response.json({ results: r.results ?? [], meta: { changes: r.meta.changes } });
    }
    if (path === "/batch") {
      const { statements } = await request.json<{ statements: SqlStatement[] }>();
      const rs = await db.batch(statements.map((s) => db.prepare(s.sql).bind(...(s.params ?? []))));
      return Response.json(rs.map((r) => ({ results: r.results ?? [], meta: { changes: r.meta.changes } })));
    }
    return Response.json({ error: "not found" }, { status: 404 });
  } catch (err) {
    return Response.json({ error: err instanceof Error ? err.message : String(err) }, { status: 500 });
  }
}

/** `PUT|GET /runs/<run>/<artifact>` → R2. Keys outside `runs/` are refused. */
async function r2Relay(request: Request, bucket: R2Bucket): Promise<Response> {
  const key = decodeURIComponent(new URL(request.url).pathname.slice(1));
  if (!key.startsWith("runs/") || key.includes("..")) {
    return Response.json({ error: "forbidden key" }, { status: 403 });
  }
  if (request.method === "PUT") {
    await bucket.put(key, await request.arrayBuffer());
    return new Response(null, { status: 204 });
  }
  if (request.method === "GET") {
    const object = await bucket.get(key);
    return object ? new Response(object.body) : Response.json({ error: "not found" }, { status: 404 });
  }
  return Response.json({ error: "method not allowed" }, { status: 405 });
}
