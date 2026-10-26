import { env } from "cloudflare:workers";
import { beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { api, signIn } from "./helpers";

beforeEach(async () => {
  for (const t of ["users", "sessions", "packs", "blobs", "pack_blobs", "pack_viewers"]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
  const list = await env.PACKS.list();
  for (const o of list.objects) await env.PACKS.delete(o.key);
});

const hash = (c: string) => c.repeat(64);

async function pack(id: string, uploader: string, blobs: string[], committed = true) {
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values (?, ?, ?, '1.0', ?, ?)",
  )
    .bind(id, uploader, `Name ${id}`, committed ? JSON.stringify({ format: 2, pack_id: id }) : null, committed ? nowSeconds() : null)
    .run();
  for (const h of blobs) {
    await env.DB.prepare("insert or ignore into blobs (sha256, size) values (?, 5)").bind(h).run();
    await env.DB.prepare("insert into pack_blobs (pack_id, sha256) values (?, ?)").bind(id, h).run();
  }
}

const view = (packId: string, userId: string) =>
  env.DB.prepare("insert into pack_viewers (pack_id, user_id) values (?, ?)").bind(packId, userId).run();
const putBlob = (h: string, text = "hello") => env.PACKS.put(`blobs/${h}`, text);

it("viewer downloads a blob", async () => {
  await signIn("up");
  const token = await signIn("v1", "Viewer", null);
  await pack("pack-a", "up", [hash("a")]);
  await view("pack-a", "v1");
  await putBlob(hash("a"));
  const res = await api(`/blobs/${hash("a")}`, {}, token);
  expect(res.status).toBe(200);
  expect(res.headers.get("content-type")).toBe("application/octet-stream");
  expect(res.headers.get("content-length")).toBe("5");
  expect(res.headers.get("accept-ranges")).toBe("bytes");
  expect(await res.text()).toBe("hello");
});

it("blob downloads resume with range", async () => {
  const token = await signIn("up");
  await pack("pack-a", "up", [hash("a")]);
  await putBlob(hash("a"), "0123456789");
  const res = await api(`/blobs/${hash("a")}`, { headers: { range: "bytes=4-" } }, token);
  expect(res.status).toBe(206);
  expect(res.headers.get("content-range")).toBe("bytes 4-9/10");
  expect(res.headers.get("content-length")).toBe("6");
  expect(await res.text()).toBe("456789");
  const bad = await api(`/blobs/${hash("a")}`, { headers: { range: "bytes=50-" } }, token);
  expect(bad.status).toBe(416);
  await bad.text();
});

it("blob access follows pack access", async () => {
  await signIn("up");
  const token = await signIn("v1", "Viewer", null);
  await pack("pack-a", "up", [hash("a")]);
  await pack("pack-b", "up", [hash("a")]);
  await view("pack-a", "v1");
  await putBlob(hash("a"));
  expect((await api(`/blobs/${hash("a")}`, {}, token)).status).toBe(200);
  await env.DB.prepare("delete from pack_viewers where pack_id = 'pack-a' and user_id = 'v1'").run();
  const res = await api(`/blobs/${hash("a")}`, {}, token);
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("blob_gone");
});

it("blob gone when missing from storage or bad hash", async () => {
  const token = await signIn("up");
  await pack("pack-a", "up", [hash("a")]);
  const res = await api(`/blobs/${hash("a")}`, {}, token);
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("blob_gone");
  expect((await api("/blobs/XYZ", {}, token)).status).toBe(404);
});

it("manifest needs access", async () => {
  const up = await signIn("up");
  const stranger = await signIn("s1", "Stranger", null);
  const viewer = await signIn("v1", "Viewer", null);
  const owner = await signIn("ow", "Owner", "owner");
  await pack("pack-a", "up", []);
  await pack("pack-u", "up", [], false);
  await view("pack-a", "v1");
  for (const t of [up, viewer, owner]) {
    const res = await api("/packs/pack-a/manifest", {}, t);
    expect(res.status).toBe(200);
    expect(((await res.json()) as { pack_id: string }).pack_id).toBe("pack-a");
  }
  for (const [p, t] of [["pack-a", stranger], ["pack-u", up], ["nope", up], ["pack-u", owner]] as const) {
    const res = await api(`/packs/${p}/manifest`, {}, t);
    expect(res.status).toBe(404);
    expect(((await res.json()) as { error: string }).error).toBe("pack_gone");
  }
  expect((await api("/packs/pack-a/manifest")).status).toBe(401);
});

it("me lists visible packs", async () => {
  await signIn("up", "Uppy");
  await signIn("up2", "Other");
  const viewer = await signIn("v1", "Viewer", null);
  await pack("pack-a", "up", []);
  await pack("pack-b", "up2", []);
  await view("pack-a", "v1");
  const me = (await (await api("/me", {}, viewer)).json()) as { packs: unknown[] };
  expect(me.packs).toEqual([
    { id: "pack-a", name: "Name pack-a", version: "1.0", uploader: { id: "up", name: "Uppy" }, access: "viewer" },
  ]);
  const up = await signIn("up", "Uppy");
  const mine = (await (await api("/me", {}, up)).json()) as { packs: { id: string; access: string }[] };
  expect(mine.packs.map((p) => [p.id, p.access])).toEqual([["pack-a", "uploader"]]);
});

it("the owner sees other packs with owner access", async () => {
  await signIn("up", "Uppy");
  const owner = await signIn("own", "Own", "owner");
  await pack("pack-a", "up", []);
  await pack("pack-b", "up", []);
  await view("pack-b", "own");
  const me = (await (await api("/me", {}, owner)).json()) as { packs: { id: string; access: string }[] };
  expect(me.packs.map((p) => [p.id, p.access])).toEqual([["pack-a", "owner"], ["pack-b", "viewer"]]);
});

it("uncommitted packs are hidden", async () => {
  const up = await signIn("up");
  await pack("pack-u", "up", [hash("b")], false);
  await putBlob(hash("b"));
  const me = (await (await api("/me", {}, up)).json()) as { packs: unknown[] };
  expect(me.packs).toEqual([]);
  expect((await api(`/blobs/${hash("b")}`, {}, up)).status).toBe(404);
});

it("suffix ranges return the tail", async () => {
  const token = await signIn("up");
  await pack("pack-a", "up", [hash("a")]);
  await putBlob(hash("a"), "0123456789");
  const res = await api(`/blobs/${hash("a")}`, { headers: { range: "bytes=-3" } }, token);
  expect(res.status).toBe(206);
  expect(res.headers.get("content-range")).toBe("bytes 7-9/10");
  expect(res.headers.get("content-length")).toBe("3");
  expect(await res.text()).toBe("789");
});
