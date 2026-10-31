import { env } from "cloudflare:workers";
import { beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { api, signIn } from "./helpers";

beforeEach(async () => {
  for (const t of ["users", "sessions", "packs", "pack_viewers", "pack_editors"]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
});

async function setup() {
  const up = await signIn("up", "Up");
  const ed = await signIn("ed", "Ed", null);
  const vw = await signIn("vw", "Vw", null);
  const ow = await signIn("ow", "Ow", "owner");
  const st = await signIn("st", "St", null);
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values ('p1', 'up', 'Pack', '1.0', '{}', ?)",
  )
    .bind(nowSeconds())
    .run();
  await env.DB.prepare("insert into pack_editors (pack_id, user_id) values ('p1', 'ed')").run();
  await env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('p1', 'vw')").run();
  return { up, ed, vw, ow, st };
}

type Row = { last_downloaded_at: number | null; download_count: number; last_used_at: number | null };
const row = () =>
  env.DB.prepare("select last_downloaded_at, download_count, last_used_at from packs where id = 'p1'").first<Row>();

const post = (path: string, token: string, body?: unknown) =>
  api(path, { method: "POST", body: body === undefined ? undefined : JSON.stringify(body) }, token);

it("a viewer download is recorded", async () => {
  const { vw } = await setup();
  const res = await post("/packs/p1/downloaded", vw);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ recorded: true });
  const first = await row();
  expect(first!.download_count).toBe(1);
  expect(first!.last_downloaded_at).toBeGreaterThan(0);
  await post("/packs/p1/downloaded", vw);
  expect((await row())!.download_count).toBe(2);
});

it("creator, editor and owner downloads record nothing", async () => {
  const { up, ed, ow } = await setup();
  for (const token of [up, ed, ow]) {
    const res = await post("/packs/p1/downloaded", token);
    expect(res.status).toBe(200);
    expect(await res.json()).toEqual({ recorded: false });
  }
  expect(await row()).toEqual({ last_downloaded_at: null, download_count: 0, last_used_at: null });
});

it("an owner who is also a viewer records nothing", async () => {
  const { ow } = await setup();
  await env.DB.prepare("insert into pack_viewers (pack_id, user_id) values ('p1', 'ow')").run();
  const res = await post("/packs/p1/downloaded", ow);
  expect(await res.json()).toEqual({ recorded: false });
  expect((await row())!.download_count).toBe(0);
});

it("a stranger gets pack_gone", async () => {
  const { st } = await setup();
  for (const [path, body] of [
    ["/packs/p1/downloaded", undefined],
    ["/packs/p1/used", { played_at: 1000 }],
  ] as const) {
    const res = await post(path, st, body);
    expect(res.status).toBe(404);
    expect(((await res.json()) as { error: string }).error).toBe("pack_gone");
  }
});

it("used keeps the newest time", async () => {
  const { vw } = await setup();
  const a = await post("/packs/p1/used", vw, { played_at: 1000 });
  expect(await a.json()).toEqual({ recorded: true });
  await post("/packs/p1/used", vw, { played_at: 900 });
  expect((await row())!.last_used_at).toBe(1000);
});

it("used ignores a future time", async () => {
  const { vw } = await setup();
  const res = await post("/packs/p1/used", vw, { played_at: nowSeconds() + 400 });
  expect(await res.json()).toEqual({ recorded: false });
  expect((await row())!.last_used_at).toBeNull();
});

it("used rejects a bad time and ignores privileged callers", async () => {
  const { vw, up } = await setup();
  for (const played_at of [0, -5, 1.5, "10", null]) {
    const res = await post("/packs/p1/used", vw, { played_at });
    expect(res.status).toBe(400);
    expect(((await res.json()) as { error: string }).error).toBe("bad_request");
  }
  const res = await post("/packs/p1/used", up, { played_at: 1000 });
  expect(await res.json()).toEqual({ recorded: false });
  expect((await row())!.last_used_at).toBeNull();
});

it("used stores no sender", async () => {
  const { vw } = await setup();
  const snapshot = async () => {
    const tables = await env.DB.prepare(
      "select name from sqlite_master where type = 'table' and name not like 'sqlite_%' and name not like '_cf_%' and name != 'packs' order by name",
    ).all<{ name: string }>();
    const out: Record<string, unknown[]> = {};
    for (const { name } of tables.results) {
      out[name] = (await env.DB.prepare(`select * from ${name}`).all()).results;
    }
    return out;
  };
  const before = await snapshot();
  await post("/packs/p1/used", vw, { played_at: 1000 });
  expect(await snapshot()).toEqual(before);
  expect((await row())!.last_used_at).toBe(1000);
});
