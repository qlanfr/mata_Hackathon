// Edge Worker: routing, auth and container lifecycle only. Product logic lives in the Rust
// containers.
import { Container, getContainer } from "@cloudflare/containers";

interface Bindings {
  RUNNER: DurableObjectNamespace<RunnerContainer>;
  /** Shared secret for write and runner routes. Set with `wrangler secret put ADMIN_TOKEN`. */
  ADMIN_TOKEN: string;
}

/**
 * Isolated experiment runner: no internet access, no bindings, no secrets. Jobs take a few
 * minutes; callers keep it awake by polling `GET /jobs/{id}`, and `sleepAfter` covers gaps.
 */
export class RunnerContainer extends Container<Bindings> {
  defaultPort = 8080;
  sleepAfter = "20m";
  enableInternet = false;
}

export default {
  async fetch(request, env): Promise<Response> {
    const url = new URL(request.url);
    if (url.pathname === "/healthz") {
      return Response.json({ ok: true });
    }

    // Day-1 exploration route: /runner/* → runner job API. Day 2 replaces this with
    // aiopsd calling http://runner.internal through an outbound handler.
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
