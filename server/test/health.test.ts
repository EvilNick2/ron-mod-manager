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

it("lookup indexes exist", async () => {
  const rows = await env.DB.prepare("select name from sqlite_master where type = 'index' and name not like 'sqlite_%' order by name").all<{ name: string }>();
  expect(rows.results.map((r) => r.name)).toEqual(
    expect.arrayContaining(["pack_viewers_user", "publish_blobs_sha", "publishes_expires"]),
  );
});
