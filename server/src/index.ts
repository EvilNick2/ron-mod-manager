import type { Env } from "./env";
import { HttpError, fail, json } from "./http";
import {
  acceptInviteRoute,
  createUploaderInvite,
  invitePage,
  showInvite,
} from "./invites";
import { getBlob, getManifest, me } from "./packs";
import {
  beginPublish,
  cancelPublish,
  commitPublish,
  publishUrls,
  verifyPublish,
} from "./publish";
import { authCallback, authLogout, authStart, authToken } from "./signin";

type Handler = (req: Request, env: Env, url: URL) => Promise<Response> | Response;

const routes: Record<string, Handler> = {
  "GET /health": () => json({ ok: true }),
  "GET /auth/start": (_req, env, url) => authStart(env, url),
  "GET /auth/callback": (_req, env, url) => authCallback(env, url),
  "POST /auth/token": (req, env) => authToken(req, env),
  "POST /auth/logout": (req, env) => authLogout(req, env),
  "POST /invites/uploader": (req, env) => createUploaderInvite(req, env),
  "GET /me": (req, env) => me(req, env),
};

type CodeHandler = (req: Request, env: Env, code: string) => Promise<Response> | Response;

const dynamicRoutes: { method: string; pattern: RegExp; handler: CodeHandler }[] = [
  { method: "GET", pattern: /^\/invites\/([^/]+)$/, handler: showInvite },
  { method: "GET", pattern: /^\/packs\/([^/]+)\/manifest$/, handler: getManifest },
  { method: "GET", pattern: /^\/blobs\/([^/]+)$/, handler: getBlob },
  { method: "POST", pattern: /^\/invites\/([^/]+)\/accept$/, handler: acceptInviteRoute },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/publish\/begin$/, handler: beginPublish },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/urls$/, handler: publishUrls },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/verify$/, handler: verifyPublish },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/commit$/, handler: commitPublish },
  { method: "DELETE", pattern: /^\/publishes\/([^/]+)$/, handler: cancelPublish },
  { method: "GET", pattern: /^\/i\/([^/]+)$/, handler: (_req, env, code) => invitePage(env, code) },
];

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    const handler: Handler | undefined = routes[`${req.method} ${url.pathname}`];
    let dynamic: { run: () => Promise<Response> | Response } | undefined;
    if (!handler) {
      for (const r of dynamicRoutes) {
        const m = r.method === req.method ? r.pattern.exec(url.pathname) : null;
        if (m) dynamic = { run: () => r.handler(req, env, m[1]) };
      }
    }
    if (!handler && !dynamic) return fail(404, "not_found", "No such route");
    try {
      return await (handler ? handler(req, env, url) : dynamic!.run());
    } catch (err) {
      if (err instanceof HttpError) return fail(err.status, err.code, err.message, err.headers);
      console.error(err);
      return fail(500, "internal", "Internal error");
    }
  },
} satisfies ExportedHandler<Env>;
