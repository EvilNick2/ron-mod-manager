import { createScheduledController } from "cloudflare:test";
import { env } from "cloudflare:workers";
import { beforeEach, expect, it } from "vitest";
import { DAY, nowSeconds } from "../src/auth";
import { cleanup } from "../src/cleanup";
import worker from "../src/index";

beforeEach(async () => {
  for (const t of [
    "sessions", "logins", "invites", "packs", "blobs", "pack_blobs", "publishes",
    "publish_blobs", "op_counts", "cleanup_state",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
  for (const o of (await env.PACKS.list()).objects) await env.PACKS.delete(o.key);
});

const hash = (c: string) => c.repeat(64);
const key = (h: string) => `blobs/${h}`;
const put = (h: string) => env.PACKS.put(key(h), "x");
const has = async (h: string) => (await env.PACKS.head(key(h))) !== null;
const count = async (table: string) =>
  (await env.DB.prepare(`select count(*) as n from ${table}`).first<{ n: number }>())!.n;
const addBlob = (h: string) =>
  env.DB.prepare("insert into blobs (sha256, size) values (?, 1)").bind(h).run();
const addPublish = (id: string, expires: number, hashes: string[]) =>
  env.DB.batch([
    env.DB.prepare(
      "insert into publishes (id, pack_id, user_id, name, version, expires_at) values (?, 'p', 'u', 'n', '1', ?)",
    ).bind(id, expires),
    ...hashes.map((h) =>
      env.DB.prepare("insert into publish_blobs (publish_id, sha256, size) values (?, ?, 1)").bind(id, h),
    ),
  ]);

it("orphan blobs are deleted", async () => {
  await addBlob(hash("a"));
  await put(hash("a"));
  await cleanup(env, nowSeconds());
  expect(await has(hash("a"))).toBe(false);
  expect(await count("blobs")).toBe(0);
});

it("referenced blobs stay", async () => {
  await addBlob(hash("a"));
  await put(hash("a"));
  await env.DB.prepare("insert into pack_blobs (pack_id, sha256) values ('p', ?)").bind(hash("a")).run();
  await cleanup(env, nowSeconds());
  expect(await has(hash("a"))).toBe(true);
  expect(await count("blobs")).toBe(1);
});

it("a blob reused by an open publish stays", async () => {
  await addBlob(hash("a"));
  await put(hash("a"));
  await addPublish("pub", nowSeconds() + 3600, [hash("a")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("a"))).toBe(true);
  expect(await count("blobs")).toBe(1);
});

it("abandoned uploads are deleted after the publish expires", async () => {
  await put(hash("b"));
  await cleanup(env, nowSeconds());
  expect(await has(hash("b"))).toBe(false);
});

it("uploads of an open publish are not abandoned", async () => {
  await put(hash("b"));
  await addPublish("pub", nowSeconds() + 3600, [hash("b")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("b"))).toBe(true);
});

it("expired publish frees its uploads", async () => {
  await put(hash("b"));
  await addPublish("pub", nowSeconds() - 1000, [hash("b")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("b"))).toBe(false);
  expect(await count("publishes")).toBe(0);
  expect(await count("publish_blobs")).toBe(0);
});

it("expired publish keeps objects shared with an open publish", async () => {
  await put(hash("b"));
  await addPublish("old", nowSeconds() - 1000, [hash("b")]);
  await addPublish("new", nowSeconds() + 3600, [hash("b")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("b"))).toBe(true);
  expect(await count("publishes")).toBe(1);
});

it("abandoned uploads are deleted in chunks", async () => {
  for (let i = 0; i < 100; i++) await put(i.toString(16).padStart(64, "0"));
  await cleanup(env, nowSeconds());
  expect((await env.PACKS.list()).objects.length).toBe(60);
  await cleanup(env, nowSeconds());
  await cleanup(env, nowSeconds());
  expect((await env.PACKS.list()).objects.length).toBe(0);
});

it("expired rows are removed", async () => {
  const now = nowSeconds();
  await env.DB.batch([
    env.DB.prepare("insert into logins (id, app_redirect, app_state, code_challenge, expires_at) values ('l', 'r', 's', 'c', ?)").bind(now - 1),
    env.DB.prepare("insert into logins (id, app_redirect, app_state, code_challenge, expires_at) values ('l2', 'r', 's', 'c', ?)").bind(now + 100),
    env.DB.prepare("insert into invites (code, kind, created_by, expires_at) values ('i', 'uploader', 'u', ?)").bind(now - 1),
    env.DB.prepare("insert into invites (code, kind, created_by, expires_at) values ('i2', 'uploader', 'u', ?)").bind(now + 100),
    env.DB.prepare("insert into sessions (token_hash, user_id, created_at, last_used) values ('s', 'u', 1, ?)").bind(now - 91 * DAY),
    env.DB.prepare("insert into sessions (token_hash, user_id, created_at, last_used) values ('s2', 'u', 1, ?)").bind(now - DAY),
  ]);
  await cleanup(env, now);
  expect(await count("logins")).toBe(1);
  expect(await count("invites")).toBe(1);
  expect(await count("sessions")).toBe(1);
});

it("the scheduled handler runs cleanup", async () => {
  await put(hash("c"));
  await worker.scheduled(createScheduledController({ scheduledTime: Date.now() }), env);
  expect(await has(hash("c"))).toBe(false);
});

const objectCount = async () => (await env.PACKS.list()).objects.length;

it("cleanup works in chunks", async () => {
  for (let i = 0; i < 100; i++) {
    const h = i.toString(16).padStart(64, "0");
    await addBlob(h);
    await put(h);
  }
  await cleanup(env, nowSeconds());
  expect(await objectCount()).toBe(60);
  expect(await count("blobs")).toBe(60);
  await cleanup(env, nowSeconds());
  expect(await objectCount()).toBeGreaterThanOrEqual(20);
  expect(await count("blobs")).toBeGreaterThanOrEqual(20);
});

it("a publish just past its expiry keeps its uploads", async () => {
  await put(hash("b"));
  await addPublish("pub", nowSeconds() - 10, [hash("b")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("b"))).toBe(true);
  expect(await count("publishes")).toBe(1);
});

it("expired publish keeps objects a pack uses", async () => {
  await addBlob(hash("a"));
  await put(hash("a"));
  await env.DB.prepare("insert into pack_blobs (pack_id, sha256) values ('p', ?)").bind(hash("a")).run();
  await addPublish("old", nowSeconds() - 1000, [hash("a")]);
  await cleanup(env, nowSeconds());
  expect(await has(hash("a"))).toBe(true);
  expect(await count("blobs")).toBe(1);
  expect(await count("publishes")).toBe(0);
});

it("a large expired publish drains in slices and does not block others", async () => {
  const hashes = Array.from({ length: 100 }, (_, i) => i.toString(16).padStart(64, "0"));
  for (const h of hashes) await put(h);
  await addPublish("big", nowSeconds() - 2000, hashes);
  await put(hash("f"));
  await addPublish("small", nowSeconds() - 1000, [hash("f")]);
  let left = await objectCount();
  for (let run = 0; run < 3; run++) {
    await cleanup(env, nowSeconds());
    const now = await objectCount();
    expect(left - now).toBeLessThanOrEqual(40);
    left = now;
  }
  expect(left).toBe(0);
  expect(await count("publishes")).toBe(0);
  expect(await count("publish_blobs")).toBe(0);
});

it("an abandoned first publish leaves no pack row after cleanup", async () => {
  const add = (id: string, manifest: string | null) =>
    env.DB.prepare("insert into packs (id, uploader_id, name, version, manifest) values (?, 'u', 'n', '1', ?)")
      .bind(id, manifest)
      .run();
  await add("p", null);
  await add("open", null);
  await add("live", "{}");
  await addPublish("pub", nowSeconds() - 1000, []);
  await env.DB.prepare(
    "insert into publishes (id, pack_id, user_id, name, version, expires_at) values ('pub2', 'open', 'u', 'n', '1', ?)",
  ).bind(nowSeconds() + 3600).run();
  await env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('p', 'v')").run();
  await env.DB.prepare(
    "insert into invites (code, kind, pack_id, created_by, expires_at) values ('c', 'viewer', 'p', 'u', ?)",
  ).bind(nowSeconds() + 3600).run();
  await cleanup(env, nowSeconds());
  const left = await env.DB.prepare("select id from packs order by id").all<{ id: string }>();
  expect(left.results.map((r) => r.id)).toEqual(["live", "open"]);
  expect(await count("pack_viewers")).toBe(0);
  expect(await count("invites")).toBe(0);
});
