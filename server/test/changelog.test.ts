import { env } from "cloudflare:workers";
import { beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { cleanup } from "../src/cleanup";
import { api, signIn } from "./helpers";

beforeEach(async () => {
  for (const t of [
    "users", "sessions", "packs", "pack_viewers", "pack_versions", "blobs", "pack_blobs",
    "publishes", "publish_blobs",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
});

const post = (path: string, body: unknown, token: string) =>
  api(path, { method: "POST", body: JSON.stringify(body) }, token);

type Entry = { version: string; published_at: number; notes: string };

// An empty publish has no blobs, so it needs no uploads before commit.
async function publish(token: string, version: string, extra: Record<string, unknown> = {}, pack = "pack-a") {
  const begun = await post(`/packs/${pack}/publish/begin`, { name: "Pack", version, blobs: [] }, token);
  expect(begun.status).toBe(200);
  const { publish_id } = (await begun.json()) as { publish_id: string };
  const manifest = { format: 2, pack_id: pack, name: "Pack", version, status: "ok", published_at: 1, mods: [] };
  return post(`/publishes/${publish_id}/commit`, { manifest, ...extra }, token);
}

async function changelog(token: string, pack = "pack-a") {
  const res = await api(`/packs/${pack}/changelog`, {}, token);
  return { res, body: (await res.json()) as { versions: Entry[]; error?: string } };
}

const count = async (sql: string) => (await env.DB.prepare(sql).first<{ n: number }>())!.n;

it("commit_records_a_version", async () => {
  const token = await signIn("u1");
  const res = await publish(token, "1.0", { notes: "First" });
  expect(res.status).toBe(200);
  const { body } = await changelog(token);
  expect(body.versions).toHaveLength(1);
  expect(body.versions[0]).toMatchObject({ version: "1.0", notes: "First" });
  expect(Math.abs(body.versions[0].published_at - nowSeconds())).toBeLessThan(10);
});

it("commit_without_notes_still_works", async () => {
  const token = await signIn("u1");
  expect((await publish(token, "1.0")).status).toBe(200);
  const { body } = await changelog(token);
  expect(body.versions.map((v) => [v.version, v.notes])).toEqual([["1.0", ""]]);
});

it("changelog_is_newest_first", async () => {
  const token = await signIn("u1");
  await publish(token, "1.0", { notes: "a" });
  await publish(token, "1.1", { notes: "b" });
  await publish(token, "1.2", { notes: "c" });
  const { body } = await changelog(token);
  expect(body.versions.map((v) => v.version)).toEqual(["1.2", "1.1", "1.0"]);
});

it("history_only_seeds_an_empty_pack", async () => {
  const token = await signIn("u1");
  const history = [
    { version: "0.1", published_at: 10, notes: "old" },
    { version: "0.2", published_at: 20, notes: "newer" },
  ];
  await publish(token, "1.0", { notes: "n", history });
  let { body } = await changelog(token);
  expect(body.versions.map((v) => v.version)).toEqual(["1.0", "0.2", "0.1"]);
  expect(body.versions[2]).toEqual(history[0]);
  await publish(token, "1.1", { history: [{ version: "x", published_at: 1, notes: "" }] });
  ({ body } = await changelog(token));
  expect(body.versions.map((v) => v.version)).toEqual(["1.1", "1.0", "0.2", "0.1"]);
});

it("keeps_newest_100", async () => {
  const token = await signIn("u1");
  const history = Array.from({ length: 99 }, (_, i) => ({ version: `h${i}`, published_at: i, notes: "" }));
  await publish(token, "1.0", { history });
  expect(await count("select count(*) as n from pack_versions")).toBe(100);
  await publish(token, "1.1");
  const { body } = await changelog(token);
  expect(body.versions).toHaveLength(100);
  expect(body.versions[0].version).toBe("1.1");
  expect(body.versions[99].version).toBe("h1");
});

it("oversized_notes_refused_before_writing", async () => {
  const token = await signIn("u1");
  const bad: Record<string, unknown>[] = [
    { notes: "x".repeat(32001) },
    { history: Array.from({ length: 100 }, () => ({ version: "v", published_at: 1, notes: "" })) },
    { history: [{ version: "v", published_at: 1, notes: "x".repeat(1000001) }] },
    { history: [{ version: "v", published_at: 1.5, notes: "" }] },
    { notes: 5 },
  ];
  for (const extra of bad) {
    const res = await publish(token, "1.0", extra);
    expect(res.status).toBe(400);
    expect(((await res.json()) as { error: string }).error).toBe("bad_changelog");
  }
  expect(await count("select count(*) as n from pack_versions")).toBe(0);
  expect(await count("select count(*) as n from packs where manifest is not null")).toBe(0);
  expect(await count("select count(*) as n from publishes")).toBe(5);
});

it("viewer_can_read_stranger_cannot", async () => {
  const up = await signIn("u1");
  const viewer = await signIn("v1", "V", null);
  const stranger = await signIn("s1", "S", null);
  await publish(up, "1.0", { notes: "hi" });
  await env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('pack-a', 'v1')").run();
  expect((await changelog(viewer)).body.versions).toHaveLength(1);
  const denied = await changelog(stranger);
  expect(denied.res.status).toBe(404);
  expect(denied.body.error).toBe("pack_gone");
});

it("deleting_pack_deletes_versions", async () => {
  const up = await signIn("u1");
  const owner = await signIn("own", "Own", "owner");
  await publish(up, "1.0", { notes: "a" });
  await publish(up, "1.0", { notes: "b" }, "pack-b");
  await publish(up, "1.0", { notes: "c" }, "pack-c");
  expect(await count("select count(*) as n from pack_versions")).toBe(3);

  expect((await api("/packs/pack-a", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from pack_versions where pack_id = 'pack-a'")).toBe(0);
  expect(await count("select count(*) as n from pack_versions")).toBe(2);

  expect((await api("/admin/users/u1", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from pack_versions")).toBe(0);

  await env.DB.batch([
    env.DB.prepare("insert into packs (id, uploader_id, name, version) values ('ghost', 'u1', 'G', '1')"),
    env.DB.prepare("insert into pack_versions (pack_id, version, published_at, notes) values ('ghost', '1', 1, '')"),
  ]);
  await cleanup(env, nowSeconds());
  expect(await count("select count(*) as n from packs where id = 'ghost'")).toBe(0);
  expect(await count("select count(*) as n from pack_versions")).toBe(0);
});
