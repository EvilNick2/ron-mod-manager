import type { Env } from "./env";
import { HttpError } from "./http";

export interface User {
  id: string;
  name: string;
  role: "owner" | "uploader" | null;
}

const DAY = 86400;
const IDLE_EXPIRY = 90 * DAY;

const nowSeconds = () => Math.floor(Date.now() / 1000);

export function randomToken(): string {
  const bytes = crypto.getRandomValues(new Uint8Array(32));
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

export async function sha256Hex(s: string): Promise<string> {
  const digest = await crypto.subtle.digest("SHA-256", new TextEncoder().encode(s));
  return [...new Uint8Array(digest)].map((b) => b.toString(16).padStart(2, "0")).join("");
}

export async function createSession(env: Env, userId: string): Promise<string> {
  const token = randomToken();
  const now = nowSeconds();
  await env.DB.prepare(
    "insert into sessions (token_hash, user_id, created_at, last_used) values (?, ?, ?, ?)",
  )
    .bind(await sha256Hex(token), userId, now, now)
    .run();
  return token;
}

const signedOut = () => new HttpError(401, "signed_out", "Sign in again");

export async function requireUser(req: Request, env: Env): Promise<User> {
  const match = /^Bearer (.+)$/.exec(req.headers.get("authorization") ?? "");
  if (!match) throw signedOut();

  const tokenHash = await sha256Hex(match[1]);
  const row = await env.DB.prepare(
    `select u.id, u.name, u.role, s.last_used
     from sessions s join users u on u.id = s.user_id
     where s.token_hash = ?`,
  )
    .bind(tokenHash)
    .first<User & { last_used: number }>();
  if (!row) throw signedOut();

  const now = nowSeconds();
  if (now - row.last_used > IDLE_EXPIRY) throw signedOut();
  if (now - row.last_used > DAY) {
    await env.DB.prepare("update sessions set last_used = ? where token_hash = ?")
      .bind(now, tokenHash)
      .run();
  }

  const user: User = { id: row.id, name: row.name, role: row.role };
  const { success } = await env.USER_LIMIT.limit({ key: user.id });
  if (!success) {
    throw new HttpError(429, "rate_limited", "Too many requests", { "retry-after": "60" });
  }
  return user;
}
