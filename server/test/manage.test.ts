import { env } from "cloudflare:workers";
import { afterEach, beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { api, signIn } from "./helpers";

const realMax = env.MAX_USERS;

beforeEach(async () => {
  for (const t of [
    "users", "sessions", "invites", "packs", "pack_viewers", "pack_blobs",
    "publishes", "publish_blobs", "blobs",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
});

afterEach(() => {
  (env as { MAX_USERS: string }).MAX_USERS = realMax;
});

async function pack(id: string, uploader: string) {
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values (?, ?, 'Pack', '1.0', '{}', ?)",
  )
    .bind(id, uploader, nowSeconds())
    .run();
}

const view = (packId: string, userId: string) =>
  env.DB.prepare("insert into pack_viewers (pack_id, user_id) values (?, ?)").bind(packId, userId).run();

const count = (sql: string, ...args: unknown[]) =>
  env.DB.prepare(sql).bind(...args).first<{ n: number }>().then((r) => r!.n);

const invite = (id: string, token: string, suffix = "invite") =>
  api(`/packs/${id}/${suffix}`, { method: "POST" }, token);

type InviteBody = { url: string; expires_at: number };

it("viewer invite is reused until it expires", async () => {
  const up = await signIn("up");
  await pack("pack-a", "up");
  const a = (await (await invite("pack-a", up)).json()) as InviteBody;
  const b = (await (await invite("pack-a", up)).json()) as InviteBody;
  expect(a.url).toMatch(/^https:\/\/packs\.example\.com\/i\//);
  expect(b).toEqual(a);
  expect(a.expires_at).toBeGreaterThan(nowSeconds() + 86000);
  expect(await count("select count(*) as n from invites where kind = 'viewer' and used_at is null")).toBe(1);
});

it("reset kills the old link", async () => {
  const up = await signIn("up");
  const v = await signIn("v", "V", null);
  await pack("pack-a", "up");
  const a = (await (await invite("pack-a", up)).json()) as InviteBody;
  const b = (await (await invite("pack-a", up, "invite/reset")).json()) as InviteBody;
  expect(b.url).not.toBe(a.url);
  const old = await api(`/invites/${a.url.split("/i/")[1]}/accept`, { method: "POST" }, v);
  expect(old.status).toBe(404);
  const fresh = await api(`/invites/${b.url.split("/i/")[1]}/accept`, { method: "POST" }, v);
  expect(fresh.status).toBe(200);
});

it("expired viewer link mints a new one", async () => {
  const up = await signIn("up");
  await pack("pack-a", "up");
  await env.DB.prepare(
    "insert into invites (code, kind, pack_id, created_by, expires_at) values ('old', 'viewer', 'pack-a', 'up', ?)",
  )
    .bind(nowSeconds() - 5)
    .run();
  const body = (await (await invite("pack-a", up)).json()) as InviteBody;
  expect(body.url).not.toContain("/i/old");
  expect(body.expires_at).toBeGreaterThan(nowSeconds());
});

it("uploader removes a viewer", async () => {
  const up = await signIn("up");
  await signIn("v", "Vee", null);
  await pack("pack-a", "up");
  await view("pack-a", "v");
  const list = await api("/packs/pack-a/viewers", {}, up);
  expect(await list.json()).toEqual([{ id: "v", name: "Vee" }]);
  expect((await api("/packs/pack-a/viewers/v", { method: "DELETE" }, up)).status).toBe(200);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);
  const again = await api("/packs/pack-a/viewers/v", { method: "DELETE" }, up);
  expect(again.status).toBe(404);
  expect(((await again.json()) as { error: string }).error).toBe("user_gone");
});

it("viewer leaves", async () => {
  await signIn("up");
  const v = await signIn("v", "V", null);
  const stranger = await signIn("s", "S", null);
  await pack("pack-a", "up");
  await view("pack-a", "v");
  const no = await api("/packs/pack-a/membership", { method: "DELETE" }, stranger);
  expect(no.status).toBe(404);
  expect((await api("/packs/pack-a/membership", { method: "DELETE" }, v)).status).toBe(200);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);
  const up = await signIn("up");
  const leave = await api("/packs/pack-a/membership", { method: "DELETE" }, up);
  expect(leave.status).toBe(400);
  expect(((await leave.json()) as { error: string }).error).toBe("own_pack");
});

it("only the pack's uploader manages it", async () => {
  await signIn("up");
  const other = await signIn("other", "O", "uploader");
  const v = await signIn("v", "V", null);
  const owner = await signIn("own", "Own", "owner");
  await pack("pack-a", "up");
  await view("pack-a", "v");
  for (const token of [other, v]) {
    expect((await invite("pack-a", token)).status).toBe(403);
    expect((await api("/packs/pack-a/viewers", {}, token)).status).toBe(403);
    expect((await api("/packs/pack-a", { method: "DELETE" }, token)).status).toBe(403);
  }
  expect((await invite("nope", owner)).status).toBe(404);
  expect(((await (await invite("123", owner)).json()) as { error: string }).error).toBe("pack_gone");
  expect((await api("/packs/pack-a/viewers", {}, owner)).status).toBe(200);
  expect((await api("/packs/pack-a", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from packs")).toBe(0);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);
});

it("removing an uploader deletes their packs", async () => {
  const owner = await signIn("own", "Own", "owner");
  await signIn("up");
  await signIn("v", "V", null);
  await pack("pack-a", "up");
  await pack("pack-b", "own");
  await view("pack-a", "v");
  await view("pack-b", "up");
  await env.DB.prepare("insert into pack_blobs (pack_id, sha256) values ('pack-a', 'x')").run();
  await env.DB.prepare(
    "insert into publishes (id, pack_id, user_id, name, version, expires_at) values ('pub', 'pack-c', 'up', 'n', '1', ?)",
  ).bind(nowSeconds() + 100).run();
  await env.DB.prepare("insert into publish_blobs (publish_id, sha256, size) values ('pub', 'x', 1)").run();

  const list = await api("/admin/users", {}, owner);
  expect(await list.json()).toEqual([
    { id: "own", name: "Own", role: "owner", packs: 1 },
    { id: "up", name: "Tester", role: "uploader", packs: 1 },
    { id: "v", name: "V", role: null, packs: 0 },
  ]);

  expect((await api("/admin/users/up", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from packs where id = 'pack-a'")).toBe(0);
  expect(await count("select count(*) as n from pack_viewers where pack_id = 'pack-a'")).toBe(0);
  expect(await count("select count(*) as n from pack_blobs")).toBe(0);
  expect(await count("select count(*) as n from publishes where expires_at <= ?", nowSeconds())).toBe(1);
  expect(await count("select count(*) as n from publish_blobs")).toBe(1);
  expect(await count("select count(*) as n from pack_viewers where user_id = 'up'")).toBe(0);
  expect(await count("select count(*) as n from sessions where user_id = 'up'")).toBe(0);
  expect(await count("select count(*) as n from packs where id = 'pack-b'")).toBe(1);
  expect(await count("select count(*) as n from users where id = 'up' and role is null")).toBe(1);
  const gone = await api("/admin/users/nobody", { method: "DELETE" }, owner);
  expect(gone.status).toBe(404);
  expect(((await gone.json()) as { error: string }).error).toBe("user_gone");
});

it("owner cannot remove themselves", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up");
  const res = await api("/admin/users/own", { method: "DELETE" }, owner);
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: string }).error).toBe("self_remove");
  expect((await api("/admin/users", {}, up)).status).toBe(403);
  expect((await api("/admin/users/own", { method: "DELETE" }, up)).status).toBe(403);
});

it("removed people free a slot", async () => {
  (env as { MAX_USERS: string }).MAX_USERS = "2";
  const owner = await signIn("own", "Own", "owner");
  await signIn("up");
  const late = await signIn("late", "Late", null);
  const mint = (await (await api("/invites/uploader", { method: "POST" }, owner)).json()) as InviteBody;
  const code = mint.url.split("/i/")[1];
  const full = await api(`/invites/${code}/accept`, { method: "POST" }, late);
  expect(full.status).toBe(403);
  expect((await api("/admin/users/up", { method: "DELETE" }, owner)).status).toBe(200);
  const ok = await api(`/invites/${code}/accept`, { method: "POST" }, late);
  expect(ok.status).toBe(200);
});

it("a removed viewer's old link answers invite_gone", async () => {
  const up = await signIn("up");
  const v = await signIn("v", "V", null);
  await pack("pack-a", "up");
  const link = (await (await invite("pack-a", up)).json()) as InviteBody;
  const accept = () => api(`/invites/${link.url.split("/i/")[1]}/accept`, { method: "POST" }, v);
  expect((await accept()).status).toBe(200);
  const removed = await api("/packs/pack-a/viewers/v", { method: "DELETE" }, up);
  expect(await removed.json()).toEqual({ ok: true, invite_reset: true });
  const again = await accept();
  expect(again.status).toBe(404);
  expect(((await again.json()) as { error: string }).error).toBe("invite_gone");
});

it("removing a user kills the links of packs they viewed and their old token", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up");
  const v = await signIn("v", "V", null);
  await pack("pack-a", "up");
  const link = (await (await invite("pack-a", up)).json()) as InviteBody;
  const accept = () => api(`/invites/${link.url.split("/i/")[1]}/accept`, { method: "POST" }, v);
  expect((await accept()).status).toBe(200);
  expect((await api("/admin/users/v", { method: "DELETE" }, owner)).status).toBe(200);
  expect((await api("/me", {}, v)).status).toBe(401);
  const v2 = await signIn("v", "V", null);
  const again = await api(`/invites/${link.url.split("/i/")[1]}/accept`, { method: "POST" }, v2);
  expect(again.status).toBe(404);
  expect(((await again.json()) as { error: string }).error).toBe("invite_gone");
  void accept;
});
