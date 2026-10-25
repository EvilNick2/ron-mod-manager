import { requireUser } from "./auth";
import type { Env } from "./env";
import { HttpError, fail, json } from "./http";
import { authCallback, authLogout, authStart, authToken } from "./signin";

type Handler = (req: Request, env: Env, url: URL) => Promise<Response> | Response;

const routes: Record<string, Handler> = {
  "GET /health": () => json({ ok: true }),
  "GET /auth/start": (_req, env, url) => authStart(env, url),
  "GET /auth/callback": (_req, env, url) => authCallback(env, url),
  "POST /auth/token": (req, env) => authToken(req, env),
  "POST /auth/logout": (req, env) => authLogout(req, env),
  "GET /me": async (req, env) => {
    const user = await requireUser(req, env);
    return json({ ...user, packs: [] });
  },
};

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    const handler = routes[`${req.method} ${url.pathname}`];
    if (!handler) return fail(404, "not_found", "No such route");
    try {
      return await handler(req, env, url);
    } catch (err) {
      if (err instanceof HttpError) return fail(err.status, err.code, err.message, err.headers);
      console.error(err);
      return fail(500, "internal", "Internal error");
    }
  },
} satisfies ExportedHandler<Env>;
