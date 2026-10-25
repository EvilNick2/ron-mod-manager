import { env, exports } from "cloudflare:workers";
import { expect, it } from "vitest";

it("health answers", async () => {
  const res = await exports.default.fetch("https://packs.test/health");
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ ok: true });
});

it("unknown routes are json 404s", async () => {
  const res = await exports.default.fetch("https://packs.test/nope");
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("not_found");
});

it("migrations ran", async () => {
  const row = await env.DB.prepare("select count(*) as n from users").first<{ n: number }>();
  expect(row?.n).toBe(0);
});
