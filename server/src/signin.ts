import { createSession, nowSeconds, randomToken, requireUser, sha256Hex } from "./auth";
import { exchangeCode, fetchUser } from "./discord";
import type { Env } from "./env";
import { HttpError, json, readJson } from "./http";

const LOGIN_TTL = 600;

interface LoginRow {
  id: string;
  app_redirect: string;
  app_state: string;
  code_challenge: string;
  code_hash: string | null;
  user_id: string | null;
  expires_at: number;
}

function loopbackRedirect(raw: string): URL {
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    throw new HttpError(400, "bad_redirect", "redirect_uri must be a loopback address");
  }
  const ok =
    url.protocol === "http:" &&
    url.hostname === "127.0.0.1" &&
    url.port !== "" &&
    url.username === "" &&
    url.password === "";
  if (!ok) throw new HttpError(400, "bad_redirect", "redirect_uri must be http://127.0.0.1:<port>");
  return url;
}

function callbackUri(env: Env): string {
  return `${env.PUBLIC_URL}/auth/callback`;
}

function page(message: string): Response {
  const html = `<!doctype html><meta charset="utf-8"><title>Sign-in</title><p>${message} Please try again from the app.</p>`;
  return new Response(html, { status: 400, headers: { "content-type": "text/html; charset=utf-8" } });
}

export async function authStart(env: Env, url: URL): Promise<Response> {
  const state = url.searchParams.get("state");
  const challenge = url.searchParams.get("code_challenge");
  const redirect = loopbackRedirect(url.searchParams.get("redirect_uri") ?? "");
  if (!state || !challenge) {
    throw new HttpError(400, "bad_request", "state and code_challenge are required");
  }

  const id = randomToken();
  await env.DB.prepare(
    "insert into logins (id, app_redirect, app_state, code_challenge, expires_at) values (?, ?, ?, ?, ?)",
  )
    .bind(id, redirect.href, state, challenge, nowSeconds() + LOGIN_TTL)
    .run();

  const authorize = new URL("https://discord.com/oauth2/authorize");
  authorize.searchParams.set("client_id", env.DISCORD_CLIENT_ID);
  authorize.searchParams.set("response_type", "code");
  authorize.searchParams.set("scope", "identify");
  authorize.searchParams.set("redirect_uri", callbackUri(env));
  authorize.searchParams.set("state", id);
  return Response.redirect(authorize.href, 302);
}

export async function authCallback(env: Env, url: URL): Promise<Response> {
  const id = url.searchParams.get("state") ?? "";
  const code = url.searchParams.get("code");
  const login = await env.DB.prepare("select * from logins where id = ?")
    .bind(id)
    .first<LoginRow>();

  if (url.searchParams.has("error") || !code) {
    if (login) await env.DB.prepare("delete from logins where id = ?").bind(id).run();
    return page("Sign-in was cancelled.");
  }
  if (!login || login.code_hash || login.expires_at <= nowSeconds()) {
    if (login) await env.DB.prepare("delete from logins where id = ?").bind(id).run();
    return page("This sign-in expired.");
  }

  const user = await fetchUser(await exchangeCode(env, code, callbackUri(env)));
  const owner = user.id === env.OWNER_DISCORD_ID;
  await env.DB.prepare(
    `insert into users (id, name, role, created_at) values (?, ?, ?, ?)
     on conflict (id) do update set name = excluded.name,
       role = case when ? then 'owner' else role end`,
  )
    .bind(user.id, user.name, owner ? "owner" : null, nowSeconds(), owner ? 1 : 0)
    .run();

  const appCode = randomToken();
  await env.DB.prepare("update logins set code_hash = ?, user_id = ? where id = ?")
    .bind(await sha256Hex(appCode), user.id, id)
    .run();

  const target = new URL(login.app_redirect);
  target.searchParams.set("code", appCode);
  target.searchParams.set("state", login.app_state);
  return Response.redirect(target.href, 302);
}

export async function authToken(req: Request, env: Env): Promise<Response> {
  const body = await readJson<{ code?: unknown; code_verifier?: unknown }>(req);
  const badLogin = () => new HttpError(400, "bad_login", "Sign-in failed, try again");
  if (typeof body.code !== "string" || typeof body.code_verifier !== "string") throw badLogin();

  const codeHash = await sha256Hex(body.code);
  const login = await env.DB.prepare("select * from logins where code_hash = ?")
    .bind(codeHash)
    .first<LoginRow>();
  if (!login || !login.user_id || login.expires_at <= nowSeconds()) throw badLogin();

  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(body.code_verifier));
  let bin = "";
  for (const b of new Uint8Array(digest)) bin += String.fromCharCode(b);
  const challenge = btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
  if (challenge !== login.code_challenge) throw badLogin();

  const del = await env.DB.prepare("delete from logins where id = ?").bind(login.id).run();
  if (del.meta.changes !== 1) throw badLogin();

  const user = await env.DB.prepare("select id, name, role from users where id = ?")
    .bind(login.user_id)
    .first<{ id: string; name: string; role: string | null }>();
  if (!user) throw badLogin();
  return json({ token: await createSession(env, user.id), user });
}

export async function authLogout(req: Request, env: Env): Promise<Response> {
  await requireUser(req, env);
  const token = /^Bearer (.+)$/.exec(req.headers.get("authorization") ?? "")![1];
  await env.DB.prepare("delete from sessions where token_hash = ?")
    .bind(await sha256Hex(token))
    .run();
  return json({ ok: true });
}
