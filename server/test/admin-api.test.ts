import { env, exports } from "cloudflare:workers";
import { beforeEach, expect, it } from "vitest";
import { createAdminSession } from "../src/admin/session";
import { DAY, nowSeconds } from "../src/auth";
import { takeSample, storageUsed } from "../src/storage";

const TABLES = [
  "users", "sessions", "invites", "packs", "pack_versions", "pack_viewers", "pack_editors", "blobs",
  "pack_blobs", "publishes", "publish_blobs", "op_counts", "cleanup_state", "storage_samples",
  "admin_sessions",
];

let cookie = "";

beforeEach(async () => {
  for (const t of TABLES) await env.DB.prepare(`delete from ${t}`).run();
  for (const o of (await env.PACKS.list()).objects) await env.PACKS.delete(o.key);
  for (const o of (await env.EXTRA_BUCKET!.list()).objects) await env.EXTRA_BUCKET!.delete(o.key);
  await addUser(env.OWNER_DISCORD_ID, "Boss", "owner");
  cookie = `ron_admin=${await createAdminSession(env, env.OWNER_DISCORD_ID)}`;
});

function admin(path: string, method = "GET", withCookie = true): Promise<Response> {
  const headers: Record<string, string> = { origin: env.PUBLIC_URL };
  if (withCookie) headers.cookie = cookie;
  return exports.default.fetch(`https://packs.test${path}`, { method, headers });
}

const addUser = (id: string, name: string, role: string | null = "uploader") =>
  env.DB.prepare("insert into users (id, name, role, created_at) values (?, ?, ?, 1)").bind(id, name, role).run();
const addPack = (id: string, uploader: string, name = id) =>
  env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values (?, ?, ?, '1.0', '{}', 100)",
  )
    .bind(id, uploader, name)
    .run();
const addBlob = (h: string, size: number, packs: string[]) =>
  env.DB.batch([
    env.DB.prepare("insert into blobs (sha256, size) values (?, ?)").bind(h, size),
    ...packs.map((p) => env.DB.prepare("insert into pack_blobs (pack_id, sha256) values (?, ?)").bind(p, h)),
  ]);
const hash = (c: string) => c.repeat(64);
const classA = async () =>
  (await env.DB.prepare("select coalesce(sum(class_a), 0) as n from op_counts").first<{ n: number }>())!.n;
const rows = async (table: string) =>
  (await env.DB.prepare(`select count(*) as n from ${table}`).first<{ n: number }>())!.n;

async function twoPacks() {
  await addUser("u1", "Una");
  await addPack("A", "u1");
  await addPack("B", "u1");
  await addBlob(hash("a"), 100, ["A", "B"]);
  await addBlob(hash("b"), 50, ["A"]);
  await addBlob(hash("c"), 30, ["B"]);
}

it("every route refuses without a session", async () => {
  const routes: [string, string][] = [
    ["GET", "/admin/api/overview"],
    ["GET", "/admin/api/packs"],
    ["GET", "/admin/api/packs/A"],
    ["DELETE", "/admin/api/packs/A"],
    ["DELETE", "/admin/api/packs/A/hold"],
    ["GET", "/admin/api/people"],
    ["DELETE", "/admin/api/users/u1"],
    ["POST", "/admin/api/invites/uploader"],
    ["POST", "/admin/api/cleanup"],
  ];
  for (const [method, path] of routes) {
    expect((await admin(path, method, false)).status, `${method} ${path}`).toBe(401);
  }
});

it("storage adds up with shared files", async () => {
  await twoPacks();
  const overview = (await (await admin("/admin/api/overview")).json()) as any;
  expect(overview.storage.single).toBe(80);
  expect(overview.storage.shared).toBe(100);
  expect(overview.storage.waiting).toBe(0);

  const { packs, tracking_started } = (await (await admin("/admin/api/packs")).json()) as any;
  expect(typeof tracking_started).toBe("number");
  const a = packs.find((p: any) => p.id === "A");
  const b = packs.find((p: any) => p.id === "B");
  expect(a.frees).toBe(50);
  expect(a.shared_bytes).toBe(100);
  expect(a.shared_with).toEqual(["B"]);
  expect(b.shared_with).toEqual(["A"]);
  expect(a.frees + b.frees + overview.storage.shared).toBe(overview.storage.single + overview.storage.shared);
});

it("used equals the publish cap figure", async () => {
  await twoPacks();
  await env.EXTRA_BUCKET!.put("x", "0123456789");
  await takeSample(env, nowSeconds());
  const { storage } = (await (await admin("/admin/api/overview")).json()) as any;
  expect(storage.extra).toBe(10);
  expect(storage.extra_label).toBe(env.EXTRA_BUCKET_LABEL ?? "Other bucket");
  expect(storage.cap).toBe(Number(env.STORAGE_CAP_BYTES));
  expect(storage.used).toBe(await storageUsed(env));
  expect(storage.used).toBe(storage.single + storage.shared + storage.waiting + storage.extra);
});

it("a deleted pack moves to waiting", async () => {
  await twoPacks();
  await addPack("C", "u1");
  await addBlob(hash("f"), 40, ["C"]);
  const before = ((await (await admin("/admin/api/overview")).json()) as any).storage;
  const frees = (((await (await admin("/admin/api/packs")).json()) as any).packs.find((p: any) => p.id === "C")).frees;
  expect(frees).toBe(40);
  expect((await admin("/admin/api/packs/C", "DELETE")).status).toBe(200);
  const after = ((await (await admin("/admin/api/overview")).json()) as any).storage;
  expect(before.single - after.single).toBe(frees);
  expect(after.waiting - before.waiting).toBe(frees);
  expect(after.used).toBe(before.used);
});

it("an open publish is listed and clearing ends it", async () => {
  await addUser("u1", "Una");
  await addPack("A", "u1", "Alpha");
  const exp = nowSeconds() + 600;
  await env.DB.prepare(
    "insert into publishes (id, pack_id, user_id, name, version, expires_at) values ('pub', 'A', 'u1', 'Alpha', '2.0', ?)",
  )
    .bind(exp)
    .run();
  const listed = ((await (await admin("/admin/api/overview")).json()) as any).publishes;
  expect(listed).toEqual([{ pack_id: "A", name: "Alpha", version: "2.0", user: "Una", expires_at: exp }]);

  const res = await admin("/admin/api/packs/A/hold", "DELETE");
  expect(await res.json()).toEqual({ cleared: 1 });
  expect(((await (await admin("/admin/api/overview")).json()) as any).publishes).toEqual([]);
});

it("people lists editors and created packs, and the owner cannot be removed", async () => {
  await addUser("u1", "Una");
  await addUser("u2", "Vic");
  await addPack("A", "u1", "Alpha");
  await env.DB.batch([
    env.DB.prepare("insert into pack_editors (pack_id, user_id) values ('A', 'u2')"),
    env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('A', ?)").bind(env.OWNER_DISCORD_ID),
  ]);
  const body = (await (await admin("/admin/api/people")).json()) as any;
  expect(body.max_users).toBe(Number(env.MAX_USERS));
  const una = body.people.find((p: any) => p.id === "u1");
  const vic = body.people.find((p: any) => p.id === "u2");
  expect(una).toMatchObject({ name: "Una", role: "uploader", created: ["Alpha"], edits: 0, views: 0 });
  expect(vic).toMatchObject({ created: [], edits: 1, views: 0 });

  const self = await admin(`/admin/api/users/${env.OWNER_DISCORD_ID}`, "DELETE");
  expect(self.status).toBe(400);
  expect(((await self.json()) as any).error).toBe("self_remove");
  expect((await admin("/admin/api/users/u2", "DELETE")).status).toBe(200);
  expect(await rows("pack_editors")).toBe(0);
  expect((await admin("/admin/api/users/nobody", "DELETE")).status).toBe(404);
});

it("an uploader invite is minted", async () => {
  const body = (await (await admin("/admin/api/invites/uploader", "POST")).json()) as any;
  expect(body.url).toMatch(/\/i\//);
  expect(body.expires_at).toBeGreaterThan(nowSeconds());
  expect(await rows("invites")).toBe(1);
});

it("sampling is hourly and pruned", async () => {
  const now = nowSeconds();
  await takeSample(env, now);
  await takeSample(env, now + 1800);
  expect(await rows("storage_samples")).toBe(1);
  await env.DB.prepare("insert into storage_samples (taken_at, bytes, extra_bytes) values (?, 1, 0)")
    .bind(now - 32 * DAY)
    .run();
  await takeSample(env, now + 3600);
  expect(await rows("storage_samples")).toBe(2);
  const old = await env.DB.prepare("select count(*) as n from storage_samples where taken_at < ?")
    .bind(now - 31 * DAY)
    .first<{ n: number }>();
  expect(old!.n).toBe(0);
});

it("the page load costs no list", async () => {
  await takeSample(env, nowSeconds());
  const before = await classA();
  const res = await admin("/admin/api/overview");
  expect(res.status).toBe(200);
  expect(await classA()).toBe(before);
  const body = (await res.json()) as any;
  expect(body.samples).toHaveLength(1);
  const d = new Date();
  expect(body.ops).toEqual({
    used: before,
    budget: Number(env.OP_BUDGET),
    resets_at: Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1) / 1000,
  });
});

it("an overview with no sample takes one", async () => {
  await admin("/admin/api/overview");
  expect(await rows("storage_samples")).toBe(1);
});

it("cleanup reports no progress inside the grace", async () => {
  const now = nowSeconds();
  await env.DB.batch([
    env.DB.prepare(
      "insert into publishes (id, pack_id, user_id, name, version, expires_at) values ('old', 'P', 'u', 'n', '1', ?)",
    ).bind(now - 100),
    env.DB.prepare("insert into publish_blobs (publish_id, sha256, size) values ('old', ?, 7)").bind(hash("d")),
  ]);
  await takeSample(env, now);
  const first = ((await (await admin("/admin/api/overview")).json()) as any).leftovers;
  expect(first).toEqual({ files: 1, bytes: 7 });
  const res = await admin("/admin/api/cleanup", "POST");
  expect(await res.json()).toEqual({ files: 1, bytes: 7 });
});

it("cleanup frees leftovers past the grace", async () => {
  await addBlob(hash("e"), 9, []);
  await env.PACKS.put(`blobs/${hash("e")}`, "x");
  const before = ((await (await admin("/admin/api/overview")).json()) as any).leftovers;
  expect(before).toEqual({ files: 1, bytes: 9 });
  expect(await (await admin("/admin/api/cleanup", "POST")).json()).toEqual({ files: 0, bytes: 0 });
});

it("pack detail lists creator, versions, editors and viewers", async () => {
  await addUser("u1", "Una");
  await addUser("u2", "Vic");
  await addUser("u3", "Wes", null);
  await addPack("A", "u1", "Alpha");
  await env.DB.batch([
    env.DB.prepare(
      "insert into pack_versions (pack_id, version, published_at, notes, publisher) values ('A', '1.0', 100, '', 'Una')",
    ),
    env.DB.prepare(
      "insert into pack_versions (pack_id, version, published_at, notes, publisher) values ('A', '1.1', 200, '', 'Vic')",
    ),
    env.DB.prepare("insert into pack_editors (pack_id, user_id) values ('A', 'u2')"),
    env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('A', 'u3')"),
  ]);
  const res = await admin("/admin/api/packs/A");
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({
    creator: "Una",
    versions: [
      { version: "1.1", published_at: 200, publisher: "Vic" },
      { version: "1.0", published_at: 100, publisher: "Una" },
    ],
    editors: ["Vic"],
    viewers: ["Wes"],
  });
  const gone = await admin("/admin/api/packs/nope");
  expect(gone.status).toBe(404);
  expect(((await gone.json()) as any).error).toBe("pack_gone");
});
