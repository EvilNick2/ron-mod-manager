import { DAY, nowSeconds, randomToken, sha256Hex, type User } from "../auth";
import type { Env } from "../env";
import { HttpError, json } from "../http";
import pageHtml from "./page.html";

export const ADMIN_LIFETIME = 7 * DAY;

const ADMIN_COOKIE = "ron_admin";
const ADMIN_COOKIE_ATTRS = "HttpOnly; Secure; SameSite=Strict; Path=/admin";

const signedOut = () => new HttpError(401, "signed_out", "Sign in again");

export function readCookie(req: Request, name: string): string | null {
  for (const part of (req.headers.get("cookie") ?? "").split(";")) {
    const eq = part.indexOf("=");
    if (eq !== -1 && part.slice(0, eq).trim() === name) return part.slice(eq + 1).trim();
  }
  return null;
}

export async function requireAdmin(req: Request, env: Env): Promise<User> {
  const token = readCookie(req, ADMIN_COOKIE);
  if (!token) throw signedOut();

  const row = await env.DB.prepare(
    `select u.id, u.name, u.role, s.expires_at
     from admin_sessions s join users u on u.id = s.user_id
     where s.token_hash = ?`,
  )
    .bind(await sha256Hex(token))
    .first<User & { expires_at: number }>();
  if (!row || row.expires_at <= nowSeconds() || row.id !== env.OWNER_DISCORD_ID) throw signedOut();

  if (req.method !== "GET" && req.method !== "HEAD" && req.headers.get("origin") !== env.PUBLIC_URL) {
    throw new HttpError(403, "bad_origin", "This request came from another site");
  }

  const user: User = { id: row.id, name: row.name, role: row.role };
  const { success } = await env.USER_LIMIT.limit({ key: user.id });
  if (!success) {
    throw new HttpError(429, "rate_limited", "Too many requests", { "retry-after": "60" });
  }
  return user;
}

export async function createAdminSession(env: Env, userId: string): Promise<string> {
  const token = randomToken();
  await env.DB.prepare("insert into admin_sessions (token_hash, user_id, expires_at) values (?, ?, ?)")
    .bind(await sha256Hex(token), userId, nowSeconds() + ADMIN_LIFETIME)
    .run();
  return token;
}

export const adminCookie = (token: string) =>
  `${ADMIN_COOKIE}=${token}; ${ADMIN_COOKIE_ATTRS}; Max-Age=${ADMIN_LIFETIME}`;

export function adminPage(): Response {
  const nonce = randomToken();
  return new Response(pageHtml.replaceAll("__NONCE__", nonce), {
    headers: {
      "content-type": "text/html; charset=utf-8",
      "cache-control": "no-store",
      "content-security-policy": `default-src 'none'; script-src 'nonce-${nonce}'; style-src 'nonce-${nonce}'; connect-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'`,
    },
  });
}

export async function adminLogout(req: Request, env: Env): Promise<Response> {
  await requireAdmin(req, env);
  await env.DB.prepare("delete from admin_sessions where token_hash = ?")
    .bind(await sha256Hex(readCookie(req, ADMIN_COOKIE)!))
    .run();
  return json({ ok: true }, 200, {
    "set-cookie": `${ADMIN_COOKIE}=; ${ADMIN_COOKIE_ATTRS}; Max-Age=0`,
  });
}
