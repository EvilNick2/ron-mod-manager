import { env, exports } from "cloudflare:workers";
import { createSession, type User } from "../src/auth";

export async function signIn(
  id: string,
  name = "Tester",
  role: User["role"] = "uploader",
): Promise<string> {
  await env.DB.prepare(
    "insert or replace into users (id, name, role, created_at) values (?, ?, ?, ?)",
  )
    .bind(id, name, role, Math.floor(Date.now() / 1000))
    .run();
  return createSession(env, id);
}

export function api(path: string, init: RequestInit = {}, token?: string): Promise<Response> {
  const headers = new Headers(init.headers);
  if (token) headers.set("authorization", `Bearer ${token}`);
  return exports.default.fetch(`https://packs.test${path}`, { ...init, headers });
}
