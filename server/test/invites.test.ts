import { env } from "cloudflare:workers";
import { afterEach, beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { api, signIn } from "./helpers";

const realMax = env.MAX_USERS;
const setMax = (n: number) => {
  (env as { MAX_USERS: string }).MAX_USERS = String(n);
};

beforeEach(async () => {
  for (const t of ["users", "sessions", "invites", "packs", "pack_viewers"]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
});

afterEach(() => {
  (env as { MAX_USERS: string }).MAX_USERS = realMax;
});

async function mintUploader(ownerToken: string): Promise<string> {
  const res = await api("/invites/uploader", { method: "POST" }, ownerToken);
  expect(res.status).toBe(200);
  const body = (await res.json()) as { url: string; expires_at: number };
  return body.url.split("/i/")[1];
}

const accept = (code: string, token: string) =>
  api(`/invites/${code}/accept`, { method: "POST" }, token);

async function addPack(id: string, uploaderId: string): Promise<void> {
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version) values (?, ?, 'Pack', '1.0')",
  )
    .bind(id, uploaderId)
    .run();
}

async function addViewerInvite(code: string, packId: string, createdBy: string): Promise<void> {
  await env.DB.prepare(
    "insert into invites (code, kind, pack_id, created_by, expires_at) values (?, 'viewer', ?, ?, ?)",
  )
    .bind(code, packId, createdBy, nowSeconds() + 86400)
    .run();
}

const roleOf = (id: string) =>
  env.DB.prepare("select role from users where id = ?")
    .bind(id)
    .first<{ role: string | null }>()
    .then((r) => r?.role);

it("only the owner makes uploader invites", async () => {
  const uploader = await signIn("up", "Up", "uploader");
  expect((await api("/invites/uploader", { method: "POST" }, uploader)).status).toBe(403);
  const owner = await signIn("own", "Own", "owner");
  const res = await api("/invites/uploader", { method: "POST" }, owner);
  expect(res.status).toBe(200);
  const body = (await res.json()) as { url: string; expires_at: number };
  expect(body.url).toMatch(/^https:\/\/packs\.example\.com\/i\/[A-Za-z0-9_-]+$/);
  expect(body.expires_at).toBeGreaterThan(nowSeconds() + 86000);
});

it("uploader invite works once", async () => {
  const owner = await signIn("own", "Own", "owner");
  const code = await mintUploader(owner);
  const a = await signIn("a", "A", null);
  const b = await signIn("b", "B", null);

  const info = await api(`/invites/${code}`, {}, a);
  expect(((await info.json()) as { kind: string; pack: unknown }).pack).toBeNull();

  const first = await accept(code, a);
  expect(first.status).toBe(200);
  expect(await first.json()).toEqual({ kind: "uploader", pack_id: null });
  expect(await roleOf("a")).toBe("uploader");

  const second = await accept(code, b);
  expect(second.status).toBe(404);
  expect(((await second.json()) as { error: string }).error).toBe("invite_gone");
  expect(await roleOf("b")).toBeNull();
  expect((await api(`/invites/${code}`, {}, b)).status).toBe(404);
});

it("expired invites are gone", async () => {
  const owner = await signIn("own", "Own", "owner");
  const code = await mintUploader(owner);
  await env.DB.prepare("update invites set expires_at = ? where code = ?")
    .bind(nowSeconds() - 1, code)
    .run();
  const a = await signIn("a", "A", null);
  expect((await api(`/invites/${code}`, {}, a)).status).toBe(404);
  const res = await accept(code, a);
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("invite_gone");
});

it("server_full past the cap", async () => {
  const owner = await signIn("own", "Own", "owner");
  await signIn("up", "Up", "uploader");
  setMax(2);
  const code = await mintUploader(owner);
  const c = await signIn("c", "C", null);
  const res = await accept(code, c);
  expect(res.status).toBe(403);
  expect(((await res.json()) as { error: string }).error).toBe("server_full");
  expect(await roleOf("c")).toBeNull();

  // the refused accept did not burn the invite
  setMax(3);
  expect((await accept(code, c)).status).toBe(200);
});

it("people with no access do not count", async () => {
  const owner = await signIn("own", "Own", "owner");
  await signIn("nobody1", "N1", null);
  await signIn("nobody2", "N2", null);
  await signIn("nobody3", "N3", null);
  setMax(2);
  const code = await mintUploader(owner);
  const c = await signIn("c", "C", null);
  expect((await accept(code, c)).status).toBe(200);
});

it("accepting twice is harmless", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up", "Up", "uploader");
  setMax(2);
  const code = await mintUploader(owner);

  const res = await accept(code, up);
  expect(res.status).toBe(200);
  expect(await roleOf("up")).toBe("uploader");
  const used = await env.DB.prepare("select used_at from invites where code = ?")
    .bind(code)
    .first<{ used_at: number | null }>();
  expect(used?.used_at).not.toBeNull();

  // the owner accepting a fresh invite keeps the owner role
  const code2 = await mintUploader(owner);
  expect((await accept(code2, owner)).status).toBe(200);
  expect(await roleOf("own")).toBe("owner");
});

it("invite page needs no sign-in", async () => {
  const res = await api("/i/abc_DEF-123");
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toContain("text/html");
  const html = await res.text();
  expect(html).toContain("Paste this link into RON Mod Manager to join.");
  expect(html).toContain("https://packs.example.com/i/abc_DEF-123");
});

it("invite page rejects odd codes", async () => {
  const res = await api("/i/" + encodeURIComponent("<script>"));
  expect(res.status).toBe(404);
  expect(await res.text()).not.toContain("<script>");
});

it("viewer invite adds the viewer", async () => {
  await signIn("up", "Up", "uploader");
  await addPack("pack-1", "up");
  await addViewerInvite("vcode", "pack-1", "up");
  const v = await signIn("v", "Vee", null);

  const info = await api("/invites/vcode", {}, v);
  expect(await info.json()).toMatchObject({
    kind: "viewer",
    pack: { id: "pack-1", name: "Pack", version: "1.0", uploader: { id: "up", name: "Up" } },
  });

  const res = await accept("vcode", v);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ kind: "viewer", pack_id: "pack-1" });
  const row = await env.DB.prepare("select 1 as x from pack_viewers where pack_id = ? and user_id = ?")
    .bind("pack-1", "v")
    .first();
  expect(row).not.toBeNull();
  expect(await roleOf("v")).toBeNull();
});

it("viewer invite is reusable", async () => {
  await signIn("up", "Up", "uploader");
  await addPack("pack-1", "up");
  await addViewerInvite("vcode", "pack-1", "up");
  const v1 = await signIn("v1", "V1", null);
  const v2 = await signIn("v2", "V2", null);
  expect((await accept("vcode", v1)).status).toBe(200);
  expect((await accept("vcode", v2)).status).toBe(200);
  expect((await accept("vcode", v1)).status).toBe(200);
  const count = await env.DB.prepare("select count(*) as n from pack_viewers").first<{ n: number }>();
  expect(count?.n).toBe(2);
  const used = await env.DB.prepare("select used_at from invites where code = 'vcode'").first<{
    used_at: number | null;
  }>();
  expect(used?.used_at).toBeNull();
});

it("pack owner and the owner accepting a viewer invite get no viewer row", async () => {
  const owner = await signIn("own", "Own", "owner");
  const up = await signIn("up", "Up", "uploader");
  await addPack("pack-1", "up");
  await addViewerInvite("vcode", "pack-1", "up");
  setMax(2);
  expect((await accept("vcode", up)).status).toBe(200);
  expect((await accept("vcode", owner)).status).toBe(200);
  const count = await env.DB.prepare("select count(*) as n from pack_viewers").first<{ n: number }>();
  expect(count?.n).toBe(0);
});

it("viewer invite respects the cap", async () => {
  await signIn("up", "Up", "uploader");
  await addPack("pack-1", "up");
  await addViewerInvite("vcode", "pack-1", "up");
  setMax(1);
  const v = await signIn("v", "V", null);
  const res = await accept("vcode", v);
  expect(res.status).toBe(403);
  expect(((await res.json()) as { error: string }).error).toBe("server_full");
});
