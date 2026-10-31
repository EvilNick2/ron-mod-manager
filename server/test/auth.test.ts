import { env } from "cloudflare:workers";
import { afterEach, expect, it, vi } from "vitest";
import { sha256Hex } from "../src/auth";
import { api, signIn } from "./helpers";

afterEach(() => {
  vi.restoreAllMocks();
});

it("me needs a token", async () => {
  const res = await api("/me");
  expect(res.status).toBe(401);
  expect(((await res.json()) as { error: string }).error).toBe("signed_out");
});

it("me returns the user", async () => {
  const token = await signIn("u1", "Alice");
  const res = await api("/me", {}, token);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ id: "u1", name: "Alice", role: "uploader", packs: [] });
});

it("idle sessions expire", async () => {
  const token = await signIn("u2");
  const old = Math.floor(Date.now() / 1000) - 91 * 86400;
  await env.DB.prepare("update sessions set last_used = ? where token_hash = ?")
    .bind(old, await sha256Hex(token))
    .run();
  const res = await api("/me", {}, token);
  expect(res.status).toBe(401);
  expect(((await res.json()) as { error: string }).error).toBe("signed_out");
});

it("rate limited users get 429", async () => {
  const token = await signIn("u3");
  vi.spyOn(env.USER_LIMIT, "limit").mockResolvedValue({ success: false });
  const res = await api("/me", {}, token);
  expect(res.status).toBe(429);
  expect(res.headers.get("retry-after")).toBe("60");
  expect(((await res.json()) as { error: string }).error).toBe("rate_limited");
});

it("sessions store only the token hash", async () => {
  const token = await signIn("u4");
  const hash = await sha256Hex(token);
  const rows = await env.DB.prepare("select token_hash from sessions where user_id = ?")
    .bind("u4")
    .all<{ token_hash: string }>();
  expect(rows.results.map((r) => r.token_hash)).toEqual([hash]);
  expect(hash).not.toBe(token);
});

it("last_used refreshes only when older than a day", async () => {
  const token = await signIn("u5");
  const hash = await sha256Hex(token);
  const read = async () =>
    (await env.DB.prepare("select last_used from sessions where token_hash = ?")
      .bind(hash)
      .first<{ last_used: number }>())!.last_used;
  const now = Math.floor(Date.now() / 1000);

  const recent = now - 3600;
  await env.DB.prepare("update sessions set last_used = ? where token_hash = ?").bind(recent, hash).run();
  expect((await api("/me", {}, token)).status).toBe(200);
  expect(await read()).toBe(recent);

  const stale = now - 2 * 86400;
  await env.DB.prepare("update sessions set last_used = ? where token_hash = ?").bind(stale, hash).run();
  expect((await api("/me", {}, token)).status).toBe(200);
  expect(await read()).toBeGreaterThanOrEqual(now);
});
