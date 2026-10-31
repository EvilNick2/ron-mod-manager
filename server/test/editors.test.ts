import { env } from "cloudflare:workers";
import { afterEach, beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { cleanup } from "../src/cleanup";
import { api, signIn } from "./helpers";

const realMax = env.MAX_USERS;

beforeEach(async () => {
  for (const t of [
    "users", "sessions", "invites", "packs", "pack_viewers", "pack_editors", "pack_blobs",
    "pack_versions", "publishes", "publish_blobs", "blobs",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
});

afterEach(() => {
  (env as { MAX_USERS: string }).MAX_USERS = realMax;
});

async function pack(id: string, uploader: string, published = true) {
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values (?, ?, 'Pack', '1.0', ?, ?)",
  )
    .bind(id, uploader, published ? "{}" : null, published ? nowSeconds() : null)
    .run();
}

const view = (packId: string, userId: string) =>
  env.DB.prepare("insert into pack_viewers (pack_id, user_id) values (?, ?)").bind(packId, userId).run();

const edit = (packId: string, userId: string) =>
  env.DB.prepare("insert into pack_editors (pack_id, user_id) values (?, ?)").bind(packId, userId).run();

const count = (sql: string, ...args: unknown[]) =>
  env.DB.prepare(sql).bind(...args).first<{ n: number }>().then((r) => r!.n);

type InviteBody = { url: string; expires_at: number };
type MeBody = { role: string | null; packs: { id: string; access: string }[] };

async function editorCode(packId: string, token: string): Promise<string> {
  const res = await api(`/packs/${packId}/editor-invite`, { method: "POST" }, token);
  expect(res.status).toBe(200);
  const body = (await res.json()) as InviteBody;
  expect(body.expires_at).toBeGreaterThan(nowSeconds() + 86000);
  return body.url.split("/i/")[1];
}

const accept = (code: string, token: string) =>
  api(`/invites/${code}/accept`, { method: "POST" }, token);

const meOf = async (token: string) => (await (await api("/me", {}, token)).json()) as MeBody;

const errorOf = async (res: Response) => ((await res.json()) as { error: string }).error;

it("creator mints an editor invite and accepting makes an editor", async () => {
  const up = await signIn("up", "Up");
  const e = await signIn("e", "Ed", null);
  await pack("pack-a", "up");
  await view("pack-a", "e");
  const code = await editorCode("pack-a", up);

  const info = await api(`/invites/${code}`, {}, e);
  expect(await info.json()).toMatchObject({
    kind: "editor",
    pack: { id: "pack-a", name: "Pack", version: "1.0", uploader: { id: "up", name: "Up" } },
  });

  const res = await accept(code, e);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ kind: "editor", pack_id: "pack-a" });
  expect(await count("select count(*) as n from pack_editors where pack_id = 'pack-a' and user_id = 'e'")).toBe(1);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);
  const me = await meOf(e);
  expect(me.role).toBe("uploader");
  expect(me.packs).toEqual([expect.objectContaining({ id: "pack-a", access: "editor" })]);

  const again = await editorCode("pack-a", up);
  expect(again).not.toBe(code);
  const own = await accept(again, up);
  expect(await own.json()).toEqual({ kind: "editor", pack_id: "pack-a" });
  expect(await count("select count(*) as n from pack_editors where user_id = 'up'")).toBe(0);
  expect(await count("select count(*) as n from invites where code = ? and used_at is null", again)).toBe(1);
});

it("editor invites are single use", async () => {
  const up = await signIn("up");
  const a = await signIn("a", "A", null);
  const b = await signIn("b", "B", null);
  await pack("pack-a", "up");
  const code = await editorCode("pack-a", up);
  expect((await accept(code, a)).status).toBe(200);
  const second = await accept(code, b);
  expect(second.status).toBe(404);
  expect(await errorOf(second)).toBe("invite_gone");
  expect(await count("select count(*) as n from pack_editors where user_id = 'b'")).toBe(0);
});

it("only the creator or owner manages editors", async () => {
  const up = await signIn("up");
  const e = await signIn("e", "Ed");
  const v = await signIn("v", "V", null);
  const owner = await signIn("own", "Own", "owner");
  await pack("pack-a", "up");
  await edit("pack-a", "e");
  await view("pack-a", "v");
  for (const token of [e, v]) {
    const mint = await api("/packs/pack-a/editor-invite", { method: "POST" }, token);
    expect(mint.status).toBe(403);
    expect(await errorOf(mint)).toBe("forbidden");
    const list = await api("/packs/pack-a/editors", {}, token);
    expect(list.status).toBe(403);
    expect(await errorOf(list)).toBe("forbidden");
    const del = await api("/packs/pack-a/editors/e", { method: "DELETE" }, token);
    expect(del.status).toBe(403);
    expect(await errorOf(del)).toBe("forbidden");
  }
  await signIn("e2", "Ab");
  await edit("pack-a", "e2");
  for (const token of [up, owner]) {
    const list = await api("/packs/pack-a/editors", {}, token);
    expect(list.status).toBe(200);
    expect(await list.json()).toEqual([
      { id: "e2", name: "Ab" },
      { id: "e", name: "Ed" },
    ]);
  }
  expect((await api("/packs/pack-a/editor-invite", { method: "POST" }, owner)).status).toBe(200);
  expect((await api("/packs/pack-a/editors/e2", { method: "DELETE" }, owner)).status).toBe(200);
  const gone = await api("/packs/pack-a/editors/v", { method: "DELETE" }, up);
  expect(gone.status).toBe(404);
  expect(await errorOf(gone)).toBe("user_gone");
});

it("removing an editor makes them a viewer", async () => {
  const up = await signIn("up");
  const e = await signIn("e", "Ed");
  await pack("pack-a", "up");
  await pack("pack-b", "up");
  await edit("pack-a", "e");
  const later = nowSeconds() + 3600;
  for (const [id, packId] of [["pub-a", "pack-a"], ["pub-b", "pack-b"]]) {
    await env.DB.prepare(
      "insert into publishes (id, pack_id, user_id, name, version, expires_at) values (?, ?, 'e', 'n', '2', ?)",
    ).bind(id, packId, later).run();
  }

  const res = await api("/packs/pack-a/editors/e", { method: "DELETE" }, up);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ ok: true });
  expect(await count("select count(*) as n from pack_editors")).toBe(0);
  const me = await meOf(e);
  expect(me.packs).toEqual([expect.objectContaining({ id: "pack-a", access: "viewer" })]);
  expect(await count("select count(*) as n from publishes where id = 'pub-a' and expires_at <= ?", nowSeconds())).toBe(1);
  expect(await count("select count(*) as n from publishes where id = 'pub-b' and expires_at = ?", later)).toBe(1);

  const again = await api("/packs/pack-a/editors/e", { method: "DELETE" }, up);
  expect(again.status).toBe(404);
  expect(await errorOf(again)).toBe("user_gone");
});

it("editors manage viewers but not the pack", async () => {
  await signIn("up");
  const e = await signIn("e", "Ed");
  await signIn("v", "Vee", null);
  await pack("pack-a", "up");
  await edit("pack-a", "e");
  await view("pack-a", "v");

  const list = await api("/packs/pack-a/viewers", {}, e);
  expect(list.status).toBe(200);
  expect(await list.json()).toEqual([{ id: "v", name: "Vee" }]);
  const link = await api("/packs/pack-a/invite", { method: "POST" }, e);
  expect(link.status).toBe(200);
  expect(((await link.json()) as InviteBody).url).toMatch(/\/i\//);
  expect((await api("/packs/pack-a/viewers/v", { method: "DELETE" }, e)).status).toBe(200);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);

  const reset = await api("/packs/pack-a/invite/reset", { method: "POST" }, e);
  expect(reset.status).toBe(403);
  expect(await errorOf(reset)).toBe("forbidden");
  const del = await api("/packs/pack-a", { method: "DELETE" }, e);
  expect(del.status).toBe(403);
  expect(await count("select count(*) as n from packs")).toBe(1);
});

it("accepting an editor invite counts towards MAX_USERS", async () => {
  const up = await signIn("up");
  await signIn("own", "Own", "owner");
  (env as { MAX_USERS: string }).MAX_USERS = "2";
  await pack("pack-a", "up");
  const code = await editorCode("pack-a", up);
  const late = await signIn("late", "Late", null);
  const res = await accept(code, late);
  expect(res.status).toBe(403);
  expect(await errorOf(res)).toBe("server_full");
  expect(await count("select count(*) as n from pack_editors")).toBe(0);
  expect(await count("select count(*) as n from invites where code = ? and used_at is null", code)).toBe(1);
});

it("deleting a pack or removing a user clears editor rows", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up");
  await signIn("e", "Ed");
  await pack("pack-a", "up");
  await pack("pack-b", "own");
  await pack("pack-c", "own");
  await pack("ghost", "own", false);
  await edit("pack-a", "e");
  await edit("pack-b", "e");
  await edit("pack-c", "up");
  await edit("ghost", "e");

  expect((await api("/packs/pack-a", { method: "DELETE" }, up)).status).toBe(200);
  expect(await count("select count(*) as n from pack_editors where pack_id = 'pack-a'")).toBe(0);

  expect((await api("/admin/users/e", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from pack_editors where user_id = 'e'")).toBe(0);

  expect((await api("/admin/users/up", { method: "DELETE" }, owner)).status).toBe(200);
  expect(await count("select count(*) as n from pack_editors")).toBe(0);

  await edit("ghost", "own");
  await cleanup(env, nowSeconds());
  expect(await count("select count(*) as n from pack_editors")).toBe(0);
  expect(await count("select count(*) as n from packs where id = 'ghost'")).toBe(0);
});

it("removing a user who edits a pack rotates its viewer invite", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up");
  const e = await signIn("e", "Ed");
  await pack("pack-a", "up");
  await edit("pack-a", "e");
  const minted = (await (await api("/packs/pack-a/invite", { method: "POST" }, up)).json()) as InviteBody;
  const link = (await (await api("/packs/pack-a/invite", { method: "POST" }, e)).json()) as InviteBody;
  expect(link).toEqual(minted);
  expect((await api("/admin/users/e", { method: "DELETE" }, owner)).status).toBe(200);
  const late = await signIn("late", "Late", null);
  const res = await accept(link.url.split("/i/")[1], late);
  expect(res.status).toBe(404);
  expect(await errorOf(res)).toBe("invite_gone");
});

it("the owner accepting an editor invite becomes an editor", async () => {
  const up = await signIn("up");
  const owner = await signIn("own", "Own", "owner");
  await pack("pack-a", "up");
  const code = await editorCode("pack-a", up);
  const res = await accept(code, owner);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ kind: "editor", pack_id: "pack-a" });
  expect(await count("select count(*) as n from pack_editors where user_id = 'own'")).toBe(1);
  expect(await count("select count(*) as n from invites where code = ? and used_at is null", code)).toBe(0);
  expect(await count("select count(*) as n from users where id = 'own' and role = 'owner'")).toBe(1);
});

it("the creator accepting their own editor invite is a no-op", async () => {
  const up = await signIn("up");
  await pack("pack-a", "up");
  const code = await editorCode("pack-a", up);
  const res = await accept(code, up);
  expect(res.status).toBe(200);
  expect(await count("select count(*) as n from pack_editors")).toBe(0);
  expect(await count("select count(*) as n from invites where code = ? and used_at is null", code)).toBe(1);
});

it("an editor accepting another editor invite is a no-op", async () => {
  const up = await signIn("up");
  const e = await signIn("e", "Ed");
  await pack("pack-a", "up");
  await edit("pack-a", "e");
  const code = await editorCode("pack-a", up);
  const res = await accept(code, e);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ kind: "editor", pack_id: "pack-a" });
  expect(await count("select count(*) as n from pack_editors")).toBe(1);
  expect(await count("select count(*) as n from invites where code = ? and used_at is null", code)).toBe(1);
});

it("an editor can leave a pack", async () => {
  await signIn("up");
  const e = await signIn("e", "Ed");
  await pack("pack-a", "up");
  await pack("pack-b", "up");
  await edit("pack-a", "e");
  const later = nowSeconds() + 3600;
  for (const [id, packId] of [["pub-a", "pack-a"], ["pub-b", "pack-b"]]) {
    await env.DB.prepare(
      "insert into publishes (id, pack_id, user_id, name, version, expires_at) values (?, ?, 'e', 'n', '2', ?)",
    ).bind(id, packId, later).run();
  }
  const res = await api("/packs/pack-a/membership", { method: "DELETE" }, e);
  expect(res.status).toBe(200);
  expect(await count("select count(*) as n from pack_editors")).toBe(0);
  expect(await count("select count(*) as n from pack_viewers")).toBe(0);
  expect((await meOf(e)).packs).toEqual([]);
  expect(await count("select count(*) as n from publishes where id = 'pub-a' and expires_at <= ?", nowSeconds())).toBe(1);
  expect(await count("select count(*) as n from publishes where id = 'pub-b' and expires_at = ?", later)).toBe(1);
  const again = await api("/packs/pack-a/membership", { method: "DELETE" }, e);
  expect(again.status).toBe(404);
});

it("viewer routes name the creator or an editor when refusing", async () => {
  await signIn("up");
  const v = await signIn("v", "V", null);
  await pack("pack-a", "up");
  const res = await api("/packs/pack-a/viewers", {}, v);
  expect(res.status).toBe(403);
  expect(((await res.json()) as { message: string }).message).toBe(
    "Only the pack's creator or an editor can do this",
  );
});
