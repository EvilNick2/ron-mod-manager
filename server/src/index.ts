import {
  adminCleanup,
  adminClearHold,
  adminDeletePack,
  adminDeleteUser,
  adminInviteUploader,
  adminOverview,
  adminPackDetail,
  adminPacks,
  adminPeople,
} from "./admin/api";
import { adminLogout, adminPage } from "./admin/session";
import { nowSeconds } from "./auth";
import { cleanup } from "./cleanup";
import type { Env } from "./env";
import { HttpError, fail, json } from "./http";
import {
  acceptInviteRoute,
  createUploaderInvite,
  invitePage,
  showInvite,
} from "./invites";
import {
  editorInvite,
  leavePack,
  listEditors,
  listUsers,
  listViewers,
  packInvite,
  removeEditor,
  removePack,
  removeUser,
  removeViewer,
  resetPackInvite,
  clearHold,
} from "./manage";
import { getBlob, getChangelog, getManifest, me } from "./packs";
import {
  beginPublish,
  cancelPublish,
  commitPublish,
  publishUrls,
  verifyPublish,
} from "./publish";
import { takeSample } from "./storage";
import { recordDownload, recordUse } from "./usage";
import { adminLogin, authCallback, authLogout, authStart, authToken } from "./signin";

type Handler = (req: Request, env: Env, url: URL) => Promise<Response> | Response;

const routes: Record<string, Handler> = {
  "GET /health": () => json({ ok: true }),
  "GET /auth/start": (_req, env, url) => authStart(env, url),
  "GET /auth/callback": (req, env, url) => authCallback(req, env, url),
  "POST /auth/token": (req, env) => authToken(req, env),
  "POST /auth/logout": (req, env) => authLogout(req, env),
  "POST /invites/uploader": (req, env) => createUploaderInvite(req, env),
  "GET /me": (req, env) => me(req, env),
  "GET /admin/users": (req, env) => listUsers(req, env),
  "GET /admin": () => adminPage(),
  "GET /admin/login": (_req, env) => adminLogin(env),
  "POST /admin/api/logout": (req, env) => adminLogout(req, env),
  "GET /admin/api/overview": (req, env) => adminOverview(req, env),
  "GET /admin/api/packs": (req, env) => adminPacks(req, env),
  "GET /admin/api/people": (req, env) => adminPeople(req, env),
  "POST /admin/api/invites/uploader": (req, env) => adminInviteUploader(req, env),
  "POST /admin/api/cleanup": (req, env) => adminCleanup(req, env),
};

type CodeHandler = (req: Request, env: Env, ...params: string[]) => Promise<Response> | Response;

const dynamicRoutes: { method: string; pattern: RegExp; handler: CodeHandler }[] = [
  { method: "GET", pattern: /^\/invites\/([^/]+)$/, handler: showInvite },
  { method: "GET", pattern: /^\/packs\/([^/]+)\/manifest$/, handler: getManifest },
  { method: "GET", pattern: /^\/packs\/([^/]+)\/changelog$/, handler: getChangelog },
  { method: "GET", pattern: /^\/blobs\/([^/]+)$/, handler: getBlob },
  { method: "POST", pattern: /^\/invites\/([^/]+)\/accept$/, handler: acceptInviteRoute },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/publish\/begin$/, handler: beginPublish },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/urls$/, handler: publishUrls },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/verify$/, handler: verifyPublish },
  { method: "POST", pattern: /^\/publishes\/([^/]+)\/commit$/, handler: commitPublish },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/downloaded$/, handler: recordDownload },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/used$/, handler: recordUse },
  { method: "DELETE", pattern: /^\/publishes\/([^/]+)$/, handler: cancelPublish },
  { method: "GET", pattern: /^\/i\/([^/]+)$/, handler: (_req, env, code) => invitePage(env, code) },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/invite$/, handler: packInvite },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/invite\/reset$/, handler: resetPackInvite },
  { method: "GET", pattern: /^\/packs\/([^/]+)\/viewers$/, handler: listViewers },
  { method: "DELETE", pattern: /^\/packs\/([^/]+)\/viewers\/([^/]+)$/, handler: removeViewer },
  { method: "POST", pattern: /^\/packs\/([^/]+)\/editor-invite$/, handler: editorInvite },
  { method: "GET", pattern: /^\/packs\/([^/]+)\/editors$/, handler: listEditors },
  { method: "DELETE", pattern: /^\/packs\/([^/]+)\/editors\/([^/]+)$/, handler: removeEditor },
  { method: "DELETE", pattern: /^\/packs\/([^/]+)\/hold$/, handler: clearHold },
  { method: "DELETE", pattern: /^\/packs\/([^/]+)\/membership$/, handler: leavePack },
  { method: "DELETE", pattern: /^\/packs\/([^/]+)$/, handler: removePack },
  { method: "GET", pattern: /^\/admin\/api\/packs\/([^/]+)$/, handler: adminPackDetail },
  { method: "DELETE", pattern: /^\/admin\/api\/packs\/([^/]+)$/, handler: adminDeletePack },
  { method: "DELETE", pattern: /^\/admin\/api\/packs\/([^/]+)\/hold$/, handler: adminClearHold },
  { method: "DELETE", pattern: /^\/admin\/api\/users\/([^/]+)$/, handler: adminDeleteUser },
  { method: "DELETE", pattern: /^\/admin\/users\/([^/]+)$/, handler: removeUser },
];

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    const url = new URL(req.url);
    const handler: Handler | undefined = routes[`${req.method} ${url.pathname}`];
    let dynamic: { run: () => Promise<Response> | Response } | undefined;
    if (!handler) {
      for (const r of dynamicRoutes) {
        const m = r.method === req.method ? r.pattern.exec(url.pathname) : null;
        if (m) {
          dynamic = { run: () => r.handler(req, env, ...m.slice(1)) };
          break;
        }
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
  async scheduled(_controller: ScheduledController, env: Env): Promise<void> {
    const now = nowSeconds();
    const results = await Promise.allSettled([takeSample(env, now), cleanup(env, now)]);
    for (const r of results) if (r.status === "rejected") console.error(r.reason);
    const failed = results.find((r) => r.status === "rejected");
    if (failed) throw failed.reason;
  },
} satisfies ExportedHandler<Env>;
