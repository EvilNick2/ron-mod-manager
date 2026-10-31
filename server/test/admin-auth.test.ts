import { env, exports } from "cloudflare:workers";
import { afterEach, expect, it, vi } from "vitest";
import { ADMIN_LIFETIME, requireAdmin } from "../src/admin/session";
import { nowSeconds, sha256Hex } from "../src/auth";
import { cleanup } from "../src/cleanup";
import { HttpError } from "../src/http";
import { api, signIn } from "./helpers";

afterEach(() => {
  vi.restoreAllMocks();
});

const STRANGER = { id: "stranger-1", username: "stranger" };
const OWNER = () => ({ id: env.OWNER_DISCORD_ID, username: "boss" });

function stubDiscord(user: { id: string; username: string }) {
  return vi.spyOn(globalThis, "fetch").mockImplementation(async (input) => {
    const url = typeof input === "string" ? input : input instanceof URL ? input.href : input.url;
    if (url === "https://discord.com/api/oauth2/token") {
      return Response.json({ access_token: "discord-access" });
    }
    if (url === "https://discord.com/api/users/@me") return Response.json(user);
    return new Response("unexpected", { status: 500 });
  });
}

function raw(path: string, init: RequestInit = {}): Promise<Response> {
  return exports.default.fetch(`https://packs.test${path}`, { redirect: "manual", ...init });
}

async function startLogin(): Promise<string> {
  const res = await raw("/admin/login");
  expect(res.status).toBe(302);
  return new URL(res.headers.get("location")!).searchParams.get("state")!;
}

function callback(state: string, cookie?: string): Promise<Response> {
  return raw(`/auth/callback?code=discord-code&state=${state}`, {
    headers: cookie === undefined ? {} : { cookie },
  });
}

async function ownerToken(): Promise<string> {
  stubDiscord(OWNER());
  const state = await startLogin();
  const res = await callback(state, `ron_admin_state=${state}`);
  expect(res.status).toBe(200);
  vi.restoreAllMocks();
  return /(?:^|[ ,])ron_admin=([^;]+)/.exec(res.headers.get("set-cookie")!)![1];
}

function logout(token: string, origin: string | null = env.PUBLIC_URL): Promise<Response> {
  const headers: Record<string, string> = { cookie: `ron_admin=${token}` };
  if (origin !== null) headers.origin = origin;
  return raw("/admin/api/logout", { method: "POST", headers });
}

const count = async (table: string) =>
  (await env.DB.prepare(`select count(*) as n from ${table}`).first<{ n: number }>())!.n;

const counts = async () => ({
  users: await count("users"),
  sessions: await count("sessions"),
  admin_sessions: await count("admin_sessions"),
});

const sessionRow = async (token: string) =>
  env.DB.prepare("select * from admin_sessions where token_hash = ?")
    .bind(await sha256Hex(token))
    .first<{ token_hash: string; user_id: string; expires_at: number }>();

const errorOf = async (res: Response) => ((await res.json()) as { error: string }).error;

async function pageNonce(): Promise<string> {
  const res = await raw("/admin");
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toContain("text/html");
  const csp = res.headers.get("content-security-policy")!;
  const nonce = /script-src 'nonce-([^']+)'/.exec(csp)![1];
  expect(csp).toBe(
    `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`,
  );
  const body = await res.text();
  expect(body).not.toContain("__NONCE__");
  expect(/nonce="([^"]+)"/.exec(body)![1]).toBe(nonce);
  expect(body).toContain('href="/admin/login"');
  return nonce;
}

it("the page has a nonce csp", async () => {
  expect(await pageNonce()).not.toBe(await pageNonce());
});

it("login sets a state cookie and goes to discord", async () => {
  const res = await raw("/admin/login");
  expect(res.status).toBe(302);
  const authorize = new URL(res.headers.get("location")!);
  expect(authorize.origin + authorize.pathname).toBe("https://discord.com/oauth2/authorize");
  expect(authorize.searchParams.get("client_id")).toBe(env.DISCORD_CLIENT_ID);
  expect(authorize.searchParams.get("redirect_uri")).toBe(`${env.PUBLIC_URL}/auth/callback`);
  const state = authorize.searchParams.get("state")!;
  expect(res.headers.get("set-cookie")).toBe(
    `ron_admin_state=${state}; HttpOnly; Secure; SameSite=Lax; Path=/auth/callback; Max-Age=600`,
  );
  const row = await env.DB.prepare("select * from logins where id = ?").bind(state).first();
  expect(row).toMatchObject({
    app_redirect: "admin",
    app_state: "",
    code_challenge: "",
    code_hash: null,
    user_id: null,
  });
});

it("owner sign-in sets a strict cookie and no redirect", async () => {
  stubDiscord(OWNER());
  const before = await counts();
  const state = await startLogin();
  const res = await callback(state, `ron_admin_state=${state}`);

  expect(res.status).toBe(200);
  expect(res.headers.get("location")).toBeNull();
  const cookie = res.headers.get("set-cookie")!;
  const token = /^ron_admin=([^;]+)/.exec(cookie)![1];
  expect(cookie).toBe(
    `ron_admin=${token}; HttpOnly; Secure; SameSite=Strict; Path=/admin; Max-Age=604800`,
  );
  expect(await res.text()).toContain('<meta http-equiv="refresh" content="0;url=/admin">');

  const row = (await sessionRow(token))!;
  expect(row.token_hash).not.toBe(token);
  expect(row.user_id).toBe(env.OWNER_DISCORD_ID);
  expect(ADMIN_LIFETIME).toBe(604800);
  expect(Math.abs(row.expires_at - (nowSeconds() + ADMIN_LIFETIME))).toBeLessThan(5);

  const user = await env.DB.prepare("select role from users where id = ?")
    .bind(env.OWNER_DISCORD_ID)
    .first<{ role: string }>();
  expect(user!.role).toBe("owner");
  expect((await counts()).sessions).toBe(before.sessions);
  expect(await env.DB.prepare("select id from logins where id = ?").bind(state).first()).toBeNull();
});

it("a non-owner sign-in writes nothing", async () => {
  stubDiscord(STRANGER);
  const before = await counts();
  const state = await startLogin();
  const res = await callback(state, `ron_admin_state=${state}`);

  expect(await res.text()).toContain("Only the server owner can open this");
  expect(res.headers.get("set-cookie")).toBeNull();
  expect(await counts()).toEqual(before);
  const user = await env.DB.prepare("select id from users where id = ?").bind(STRANGER.id).first();
  expect(user).toBeNull();
});

it("the state cookie must match", async () => {
  const before = await counts();
  const spy = stubDiscord(OWNER());
  const cookieFor: ((state: string) => string | undefined)[] = [
    () => undefined,
    () => "ron_admin_state=wrong",
    () => "ron_admin_state=",
    (state) => `ron_admin_state=${state}x`,
    (state) => `xron_admin_state=${state}`,
    (state) => `ron_admin_state=${state.slice(0, -1)}`,
  ];
  for (const make of cookieFor) {
    const state = await startLogin();
    const res = await callback(state, make(state));
    expect(res.status).toBe(400);
    expect(res.headers.get("set-cookie")).toBeNull();
    expect(await res.text()).toContain("This sign-in expired.");
  }
  expect(spy).not.toHaveBeenCalled();
  expect(await counts()).toEqual(before);
});

it("a dashboard login works once", async () => {
  stubDiscord(OWNER());
  const state = await startLogin();
  expect((await callback(state, `ron_admin_state=${state}`)).status).toBe(200);
  const before = await counts();
  const again = await callback(state, `ron_admin_state=${state}`);
  expect(again.status).toBe(400);
  expect(again.headers.get("set-cookie")).toBeNull();
  expect(await counts()).toEqual(before);
});

it("a dashboard login is never redeemable for an app token", async () => {
  const state = await startLogin();
  const redeem = (code: string, code_verifier: string) =>
    api("/auth/token", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ code, code_verifier }),
    });
  for (const [code, verifier] of [[state, ""], ["", ""], [state, state]]) {
    const res = await redeem(code, verifier);
    expect(res.status).toBe(400);
    expect(await errorOf(res)).toBe("bad_login");
  }

  stubDiscord(OWNER());
  const res = await callback(state, `ron_admin_state=${state}`);
  expect(res.status).toBe(200);
  expect(await env.DB.prepare("select id from logins where id = ?").bind(state).first()).toBeNull();
});

it("the app flow cannot make a dashboard login", async () => {
  const q = new URLSearchParams({ redirect_uri: "admin", state: "s", code_challenge: "c" });
  const res = await raw(`/auth/start?${q}`);
  expect(res.status).toBe(400);
  expect(await errorOf(res)).toBe("bad_redirect");
});

it("an app login is unaffected", async () => {
  stubDiscord(OWNER());
  const before = await counts();
  const verifier = "a".repeat(50);
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(verifier));
  let bin = "";
  for (const b of new Uint8Array(digest)) bin += String.fromCharCode(b);
  const challenge = btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  const q = new URLSearchParams({
    redirect_uri: "http://127.0.0.1:53682/cb",
    state: "app-state",
    code_challenge: challenge,
  });
  const started = await raw(`/auth/start?${q}`);
  expect(started.headers.get("set-cookie")).toBeNull();
  const state = new URL(started.headers.get("location")!).searchParams.get("state")!;

  const cb = await callback(state);
  expect(cb.status).toBe(302);
  expect(cb.headers.get("set-cookie")).toBeNull();
  const loc = new URL(cb.headers.get("location")!);
  expect(loc.origin).toBe("http://127.0.0.1:53682");
  expect(loc.searchParams.get("state")).toBe("app-state");

  const tok = await api("/auth/token", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ code: loc.searchParams.get("code"), code_verifier: verifier }),
  });
  expect(tok.status).toBe(200);
  expect((await counts()).admin_sessions).toBe(before.admin_sessions);
});

it("an app token does not open the admin api", async () => {
  const token = await signIn(env.OWNER_DISCORD_ID, "boss", "owner");
  const res = await api(
    "/admin/api/logout",
    { method: "POST", headers: { origin: env.PUBLIC_URL } },
    token,
  );
  expect(res.status).toBe(401);
  expect(await errorOf(res)).toBe("signed_out");

  const asCookie = await logout(token);
  expect(asCookie.status).toBe(401);
  expect((await api("/me", {}, token)).status).toBe(200);
});

it("an admin cookie does not open app routes", async () => {
  const token = await ownerToken();
  const cookie = { cookie: `ron_admin=${token}` };
  expect((await raw("/me", { headers: cookie })).status).toBe(401);
  expect((await raw("/admin/users", { headers: cookie })).status).toBe(401);
  expect(
    (await api("/me", { headers: { authorization: `Bearer ${token}` } })).status,
  ).toBe(401);
});

it("an expired session is refused, and use does not extend it", async () => {
  const token = await ownerToken();
  const hash = await sha256Hex(token);
  const start = (await sessionRow(token))!.expires_at;
  const req = new Request("https://packs.test/admin", {
    headers: { cookie: `ron_admin=${token}` },
  });

  const user = await requireAdmin(req, env);
  expect(user.id).toBe(env.OWNER_DISCORD_ID);
  expect((await sessionRow(token))!.expires_at).toBe(start);

  await env.DB.prepare("update admin_sessions set expires_at = ? where token_hash = ?")
    .bind(nowSeconds(), hash)
    .run();
  const res = await logout(token);
  expect(res.status).toBe(401);
  expect(await errorOf(res)).toBe("signed_out");
});

it("a change with a foreign origin is refused", async () => {
  const token = await ownerToken();
  for (const origin of ["https://evil.test", null, "null", `${env.PUBLIC_URL}.evil.test`]) {
    const res = await logout(token, origin);
    expect(res.status, String(origin)).toBe(403);
    expect(await errorOf(res)).toBe("bad_origin");
    expect(await sessionRow(token)).not.toBeNull();
  }
});

const CHANGES: [string, string][] = [
  ["DELETE", "/admin/api/packs/A"],
  ["DELETE", "/admin/api/packs/A/hold"],
  ["DELETE", "/admin/api/users/u1"],
  ["POST", "/admin/api/cleanup"],
  ["POST", "/admin/api/invites/uploader"],
];

async function snapshot() {
  const out: Record<string, number> = {};
  for (const t of ["users", "packs", "publishes", "invites", "blobs", "op_counts", "sessions"]) {
    out[t] = await count(t);
  }
  return out;
}

it("every admin change refuses a foreign origin and a bearer token", async () => {
  const token = await ownerToken();
  await signIn("u1", "Una", "uploader");
  await env.DB.batch([
    env.DB.prepare(
      "insert into packs (id, uploader_id, name, version, manifest, published_at) values ('A', 'u1', 'A', '1.0', '{}', 100)",
    ),
    env.DB.prepare(
      "insert into publishes (id, pack_id, user_id, name, version, expires_at) values ('pub', 'A', 'u1', 'A', '2.0', ?)",
    ).bind(nowSeconds() + 600),
  ]);
  const before = await snapshot();
  const bearer = await signIn(env.OWNER_DISCORD_ID, "boss", "owner");
  const sessions = await snapshot();

  for (const [method, path] of CHANGES) {
    const label = `${method} ${path}`;
    for (const origin of ["https://evil.test", null, "null"]) {
      const headers: Record<string, string> = { cookie: `ron_admin=${token}` };
      if (origin !== null) headers.origin = origin;
      const res = await raw(path, { method, headers });
      expect(res.status, `${label} ${origin}`).toBe(403);
      expect(await errorOf(res)).toBe("bad_origin");
    }
    const viaBearer = await api(path, { method, headers: { origin: env.PUBLIC_URL } }, bearer);
    expect(viaBearer.status, label).toBe(401);
    expect(await errorOf(viaBearer)).toBe("signed_out");
  }
  expect(await snapshot()).toEqual(sessions);
  expect(sessions.packs).toBe(before.packs);
  expect(sessions.publishes).toBe(before.publishes);
  expect(await env.DB.prepare("select id from users where id = 'u1'").first()).not.toBeNull();
});

it("an admin cookie cannot remove a user through the app route", async () => {
  const token = await ownerToken();
  await signIn("u1", "Una", "uploader");
  const res = await raw("/admin/users/u1", {
    method: "DELETE",
    headers: { cookie: `ron_admin=${token}`, origin: env.PUBLIC_URL },
  });
  expect(res.status).toBe(401);
  expect(await env.DB.prepare("select id from users where id = 'u1'").first()).not.toBeNull();
});

it("a read does not need an origin", async () => {
  const token = await ownerToken();
  for (const method of ["GET", "HEAD"]) {
    const req = new Request("https://packs.test/admin", {
      method,
      headers: { cookie: `ron_admin=${token}` },
    });
    expect((await requireAdmin(req, env)).id).toBe(env.OWNER_DISCORD_ID);
  }
});

it("a session whose user is no longer the owner is refused", async () => {
  await signIn("old-owner", "old", "uploader");
  const token = "former-owner-token";
  await env.DB.prepare("insert into admin_sessions (token_hash, user_id, expires_at) values (?, ?, ?)")
    .bind(await sha256Hex(token), "old-owner", nowSeconds() + ADMIN_LIFETIME)
    .run();
  const res = await logout(token);
  expect(res.status).toBe(401);
  expect(await errorOf(res)).toBe("signed_out");
});

it("a similarly named cookie is not a session", async () => {
  const token = await ownerToken();
  const res = await raw("/admin/api/logout", {
    method: "POST",
    headers: { cookie: `xron_admin=${token}`, origin: env.PUBLIC_URL },
  });
  expect(res.status).toBe(401);
  expect(await sessionRow(token)).not.toBeNull();
});

it("the rate limit applies", async () => {
  const token = await ownerToken();
  vi.spyOn(env.USER_LIMIT, "limit").mockResolvedValue({ success: false });
  const req = new Request("https://packs.test/admin", {
    headers: { cookie: `ron_admin=${token}` },
  });
  const err = await requireAdmin(req, env).catch((e: unknown) => e);
  expect(err).toBeInstanceOf(HttpError);
  expect((err as HttpError).status).toBe(429);
  expect((err as HttpError).headers).toEqual({ "retry-after": "60" });
});

it("logout deletes the session and clears the cookie", async () => {
  const token = await ownerToken();
  const res = await logout(token);
  expect(res.status).toBe(200);
  expect(res.headers.get("set-cookie")).toBe(
    "ron_admin=; HttpOnly; Secure; SameSite=Strict; Path=/admin; Max-Age=0",
  );
  expect(await sessionRow(token)).toBeNull();
  expect((await logout(token)).status).toBe(401);
});

it("cleanup deletes only expired dashboard sessions", async () => {
  const live = await ownerToken();
  const dead = await ownerToken();
  const now = nowSeconds();
  await env.DB.prepare("update admin_sessions set expires_at = ? where token_hash = ?")
    .bind(now, await sha256Hex(dead))
    .run();
  await cleanup(env, now);
  expect(await sessionRow(dead)).toBeNull();
  expect(await sessionRow(live)).not.toBeNull();
});

it("no name reaches innerHTML", async () => {
  const body = await (await raw("/admin")).text();
  for (const word of ["innerHTML", "insertAdjacentHTML", "document.write", "eval"]) {
    expect(body).not.toContain(word);
  }
});

it("the page has one nonced script and one nonced style", async () => {
  const body = await (await raw("/admin")).text();
  expect(body.match(/<script\b/g)).toHaveLength(1);
  expect(body.match(/<style\b/g)).toHaveLength(1);
  expect(body.match(/<script nonce="[^"]+">/g)).toHaveLength(1);
  expect(body.match(/<style nonce="[^"]+">/g)).toHaveLength(1);
});
