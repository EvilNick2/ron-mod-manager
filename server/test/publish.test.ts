import { env } from "cloudflare:workers";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { nowSeconds } from "../src/auth";
import { cleanup } from "../src/cleanup";
import { api, signIn } from "./helpers";

const realCap = env.STORAGE_CAP_BYTES;
const realBudget = env.OP_BUDGET;
const setVar = (name: "STORAGE_CAP_BYTES" | "OP_BUDGET", v: string) => {
  (env as unknown as Record<string, string>)[name] = v;
};

beforeEach(async () => {
  for (const t of [
    "users", "sessions", "packs", "blobs", "pack_blobs", "publishes", "publish_blobs", "op_counts",
    "pack_editors", "pack_versions",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
  for (const b of [env.PACKS, env.EXTRA_BUCKET!]) {
    const list = await b.list();
    for (const o of list.objects) await b.delete(o.key);
  }
});

afterEach(() => {
  vi.useRealTimers();
  setVar("STORAGE_CAP_BYTES", realCap);
  setVar("OP_BUDGET", realBudget);
});

const classA = async () =>
  (await env.DB.prepare("select coalesce(sum(class_a), 0) as n from op_counts").first<{ n: number }>())!.n;

const hash = (c: string) => c.repeat(64);
const post = (path: string, body: unknown, token: string) =>
  api(path, { method: "POST", body: JSON.stringify(body) }, token);

const begin = (
  pack: string,
  blobs: { sha256: string; size: number }[],
  token: string,
  version = "1.0",
) => currentBase(pack).then((base) => post(`/packs/${pack}/publish/begin`, { name: "Pack", version, blobs, base }, token));

const currentBase = async (pack: string) =>
  (await env.DB.prepare("select version from packs where id = ? and published_at is not null").bind(pack).first<{
    version: string;
  }>())?.version ?? null;

async function started(
  pack: string,
  blobs: { sha256: string; size: number }[],
  token: string,
  version = "1.0",
) {
  const res = await begin(pack, blobs, token, version);
  expect(res.status).toBe(200);
  return (await res.json()) as { publish_id: string; missing: string[] };
}

const manifest = (pack: string, blobs: { sha256: string; size: number }[], version = "1.0") => ({
  format: 2,
  pack_id: pack,
  name: "Pack",
  version,
  status: "ok",
  published_at: 1,
  mods: [
    {
      id: "m1",
      name: "Mod",
      version: "1",
      hash: "h",
      addons: [],
      files: blobs.map((b, i) => ({ path: `f${i}`, ...b })),
    },
  ],
});

const put = (sha: string, size: number) => env.PACKS.put(`blobs/${sha}`, new Uint8Array(size));

it("first publish creates the pack", async () => {
  const token = await signIn("u1");
  const { publish_id, missing } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], token);
  expect(publish_id).toBeTruthy();
  expect(missing).toEqual([hash("a")]);
  const row = await env.DB.prepare("select uploader_id from packs where id = 'pack-a'").first<{
    uploader_id: string;
  }>();
  expect(row?.uploader_id).toBe("u1");
});

it("rejects bad pack ids and hashes", async () => {
  const token = await signIn("u1");
  expect((await begin("12345", [], token)).status).toBe(400);
  expect((await begin("ok", [{ sha256: "xyz", size: 1 }], token)).status).toBe(400);
});

it("another uploader cannot take a pack id", async () => {
  const t1 = await signIn("u1");
  const t2 = await signIn("u2");
  await started("pack-a", [], t1);
  const res = await begin("pack-a", [], t2);
  expect(res.status).toBe(409);
  expect(((await res.json()) as { error: string }).error).toBe("pack_taken");
});

it("only missing blobs are asked for", async () => {
  const token = await signIn("u1");
  await env.DB.prepare("insert into blobs (sha256, size) values (?, 5)").bind(hash("a")).run();
  const { missing } = await started(
    "pack-a",
    [{ sha256: hash("a"), size: 5 }, { sha256: hash("b"), size: 7 }],
    token,
  );
  expect(missing).toEqual([hash("b")]);
});

it("storage cap refuses begin", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  const res = await begin("pack-a", [{ sha256: hash("a"), size: 11 }], token);
  expect(res.status).toBe(507);
  expect(((await res.json()) as { error: string }).error).toBe("storage_full");
});

it("reservations count towards the cap", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  await started("pack-a", [{ sha256: hash("a"), size: 8 }], token);
  const res = await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token);
  expect(res.status).toBe(507);
});

it("extra bucket counts towards the cap", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  await env.EXTRA_BUCKET!.put("other", new Uint8Array(8));
  const res = await begin("pack-a", [{ sha256: hash("a"), size: 5 }], token);
  expect(res.status).toBe(507);
});

it("op budget refuses begin", async () => {
  const token = await signIn("u1");
  setVar("OP_BUDGET", "1");
  const res = await begin(
    "pack-a",
    [{ sha256: hash("a"), size: 1 }, { sha256: hash("b"), size: 1 }],
    token,
  );
  expect(res.status).toBe(429);
  expect(((await res.json()) as { error: string }).error).toBe("op_budget");
});

it("a broken cap var gives 500", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "");
  const res = await begin("pack-a", [{ sha256: hash("a"), size: 1 }], token);
  expect(res.status).toBe(500);
  expect(((await res.json()) as { error: string }).error).toBe("config");
});

it("urls are presigned for five minutes", async () => {
  const token = await signIn("u1");
  const { publish_id } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], token);
  const before = await classA();
  const res = await post(
    `/publishes/${publish_id}/urls`,
    { blobs: [hash("a")] },
    token,
  );
  expect(res.status).toBe(200);
  const { urls } = (await res.json()) as { urls: Record<string, string> };
  const u = new URL(urls[hash("a")]);
  expect(u.searchParams.get("X-Amz-Expires")).toBe("300");
  expect(u.searchParams.get("X-Amz-Signature")).toBeTruthy();
  const signed = u.searchParams.get("X-Amz-SignedHeaders");
  expect(signed).toContain("x-amz-checksum-sha256");
  expect(signed).toContain("content-length");
  expect(u.pathname).toBe(`/${env.R2_BUCKET_NAME}/blobs/${hash("a")}`);
  expect((await classA()) - before).toBe(1);
});

it("urls refuse more than 50", async () => {
  const token = await signIn("u1");
  const { publish_id } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], token);
  const blobs = Array.from({ length: 51 }, () => hash("a"));
  expect((await post(`/publishes/${publish_id}/urls`, { blobs }, token)).status).toBe(400);
});

it("verify deletes wrong sizes", async () => {
  const token = await signIn("u1");
  const { publish_id } = await started(
    "pack-a",
    [{ sha256: hash("a"), size: 5 }, { sha256: hash("b"), size: 5 }, { sha256: hash("c"), size: 5 }],
    token,
  );
  await put(hash("a"), 5);
  await put(hash("b"), 3);
  const res = await post(
    `/publishes/${publish_id}/verify`,
    { blobs: [hash("a"), hash("b"), hash("c")] },
    token,
  );
  const body = (await res.json()) as { verified: string[]; bad: string[] };
  expect(body.verified).toEqual([hash("a")]);
  expect(body.bad.sort()).toEqual([hash("b"), hash("c")]);
  expect(await env.PACKS.head(`blobs/${hash("b")}`)).toBeNull();
});

it("commit refuses unverified blobs", async () => {
  const token = await signIn("u1");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const { publish_id } = await started("pack-a", blobs, token);
  const res = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, token);
  expect(res.status).toBe(409);
  expect(((await res.json()) as { error: string }).error).toBe("not_uploaded");
});

it("commit refuses a manifest that differs from begin", async () => {
  const token = await signIn("u1");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const { publish_id } = await started("pack-a", blobs, token);
  const other = manifest("pack-a", [{ sha256: hash("a"), size: 6 }]);
  const res = await post(`/publishes/${publish_id}/commit`, { manifest: other }, token);
  expect(res.status).toBe(400);
  expect(((await res.json()) as { error: string }).error).toBe("bad_manifest");
  const wrongPack = await post(
    `/publishes/${publish_id}/commit`,
    { manifest: manifest("pack-z", blobs) },
    token,
  );
  expect(wrongPack.status).toBe(400);
});

it("commit swaps the version", async () => {
  const token = await signIn("u1");
  const v1 = [{ sha256: hash("a"), size: 5 }];
  const p1 = await started("pack-a", v1, token);
  await put(hash("a"), 5);
  await post(`/publishes/${p1.publish_id}/verify`, { blobs: [hash("a")] }, token);
  const c1 = await post(`/publishes/${p1.publish_id}/commit`, { manifest: manifest("pack-a", v1) }, token);
  expect(await c1.json()).toMatchObject({ ok: true });

  const v2 = [{ sha256: hash("b"), size: 7 }];
  const p2 = await started("pack-a", v2, token, "2.0");
  await put(hash("b"), 7);
  await post(`/publishes/${p2.publish_id}/verify`, { blobs: [hash("b")] }, token);
  const c2 = await post(
    `/publishes/${p2.publish_id}/commit`,
    { manifest: manifest("pack-a", v2, "2.0") },
    token,
  );
  expect(c2.status).toBe(200);

  const pack = await env.DB.prepare("select version, manifest, published_at from packs where id = 'pack-a'").first<{
    version: string;
    manifest: string;
    published_at: number;
  }>();
  expect(pack?.version).toBe("2.0");
  expect(JSON.parse(pack!.manifest).published_at).toBe(pack!.published_at);
  expect(pack!.published_at).toBeGreaterThan(1000);
  const links = await env.DB.prepare("select sha256 from pack_blobs where pack_id = 'pack-a'").all();
  expect(links.results.map((r) => r.sha256)).toEqual([hash("b")]);
  const blobs = await env.DB.prepare("select count(*) as n from blobs").first<{ n: number }>();
  expect(blobs?.n).toBe(2);
  const left = await env.DB.prepare("select count(*) as n from publishes").first<{ n: number }>();
  expect(left?.n).toBe(0);
});

it("viewers cannot publish", async () => {
  const token = await signIn("v1", "Viewer", null);
  const res = await begin("pack-a", [], token);
  expect(res.status).toBe(403);
  expect(((await res.json()) as { error: string }).error).toBe("forbidden");
});

it("expired publishes are gone", async () => {
  const token = await signIn("u1");
  const { publish_id } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], token);
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() - 1).run();
  const res = await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, token);
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("publish_gone");
});

it("another user sees a publish as gone and cancel expires it", async () => {
  const t1 = await signIn("u1");
  const t2 = await signIn("u2");
  const { publish_id } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], t1);
  expect((await api(`/publishes/${publish_id}`, { method: "DELETE" }, t2)).status).toBe(404);
  expect((await api(`/publishes/${publish_id}`, { method: "DELETE" }, t1)).status).toBe(200);
  const left = await env.DB.prepare("select expires_at from publishes").first<{ expires_at: number }>();
  expect(left!.expires_at).toBeLessThanOrEqual(nowSeconds());
});

it("expired reservations still count towards the cap", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  await started("pack-a", [{ sha256: hash("a"), size: 8 }], token);
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() - 1).run();
  expect((await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token)).status).toBe(507);
});

it("a shared hash across publishes counts once", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  await started("pack-a", [{ sha256: hash("a"), size: 8 }], token);
  expect((await begin("pack-b", [{ sha256: hash("a"), size: 8 }], token)).status).toBe(200);
});

it("a wrong size for a stored hash is refused and its object survives verify", async () => {
  const a = await signIn("u1");
  const b = await signIn("u2");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const p1 = await started("pack-a", blobs, a);
  await put(hash("a"), 5);
  await post(`/publishes/${p1.publish_id}/verify`, { blobs: [hash("a")] }, a);
  await post(`/publishes/${p1.publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, a);

  expect((await begin("pack-b", [{ sha256: hash("a"), size: 1 }], b)).status).toBe(400);
  const p2 = await started("pack-b", blobs, b);
  expect(p2.missing).toEqual([]);
  const res = await post(`/publishes/${p2.publish_id}/verify`, { blobs: [hash("a")] }, b);
  expect(((await res.json()) as { verified: string[] }).verified).toEqual([hash("a")]);
  expect(await env.PACKS.head(`blobs/${hash("a")}`)).not.toBeNull();
  const urls = await post(`/publishes/${p2.publish_id}/urls`, { blobs: [hash("a")] }, b);
  expect(urls.status).toBe(400);
  expect(((await urls.json()) as { error: string }).error).toBe("already_stored");
});

it("commit refuses when another user owns the pack", async () => {
  const a = await signIn("u1");
  const b = await signIn("u2");
  const { publish_id } = await started("pack-a", [], a);
  await env.DB.prepare("update packs set uploader_id = 'u2' where id = 'pack-a'").run();
  const res = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", []) }, a);
  expect(res.status).toBe(409);
  expect(((await res.json()) as { error: string }).error).toBe("pack_taken");
  void b;
});

it("a wrong size for an open reservation is refused and the object survives", async () => {
  const a = await signIn("u1");
  const b = await signIn("u2");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const pb = await started("pack-b", blobs, b);
  await put(hash("a"), 5);
  await post(`/publishes/${pb.publish_id}/verify`, { blobs: [hash("a")] }, b);
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() - 1).run();
  expect((await begin("pack-a", [{ sha256: hash("a"), size: 9 }], a)).status).toBe(400);
  expect(await env.PACKS.head(`blobs/${hash("a")}`)).not.toBeNull();
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() + 100).run();
  const c = await post(`/publishes/${pb.publish_id}/commit`, { manifest: manifest("pack-b", blobs) }, b);
  expect(c.status).toBe(200);
});

it("commit fails cleanly when the publish vanishes before its batch", async () => {
  const token = await signIn("u1");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const { publish_id } = await started("pack-a", blobs, token);
  await put(hash("a"), 5);
  await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, token);

  const real = env.DB;
  const wrapped = new Proxy(real, {
    get(target, prop) {
      if (prop === "batch") {
        return async (stmts: D1PreparedStatement[]) => {
          await real.prepare("delete from publishes where id = ?").bind(publish_id).run();
          return real.batch(stmts);
        };
      }
      const v = Reflect.get(target, prop, target);
      return typeof v === "function" ? v.bind(target) : v;
    },
  });
  (env as unknown as Record<string, unknown>).DB = wrapped;
  let res: Response;
  try {
    res = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, token);
  } finally {
    (env as unknown as Record<string, unknown>).DB = real;
  }
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("publish_gone");
  for (const t of ["blobs", "pack_blobs"]) {
    const n = await env.DB.prepare(`select count(*) as n from ${t}`).first<{ n: number }>();
    expect(n!.n).toBe(0);
  }
  const pack = await env.DB.prepare("select manifest from packs where id = 'pack-a'").first<{ manifest: string | null }>();
  expect(pack!.manifest).toBeNull();
});

it("a cancelled publish keeps counting towards the cap until cleanup runs", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  const blobs = [{ sha256: hash("a"), size: 8 }];
  const { publish_id } = await started("pack-a", blobs, token);
  expect((await api(`/publishes/${publish_id}`, { method: "DELETE" }, token)).status).toBe(200);

  for (const res of [
    await post(`/publishes/${publish_id}/urls`, { blobs: [hash("a")] }, token),
    await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, token),
    await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, token),
    await api(`/publishes/${publish_id}`, { method: "DELETE" }, token),
  ]) {
    expect(res.status).toBe(404);
    expect(((await res.json()) as { error: string }).error).toBe("publish_gone");
  }

  expect((await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token)).status).toBe(507);
  await cleanup(env, nowSeconds());
  expect((await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token)).status).toBe(507);
  await cleanup(env, nowSeconds() + 901);
  expect((await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token)).status).toBe(200);
});

it("deleting a pack during an open publish keeps its storage counted and fails the commit", async () => {
  const token = await signIn("u1");
  setVar("STORAGE_CAP_BYTES", "10");
  const blobs = [{ sha256: hash("a"), size: 8 }];
  const { publish_id } = await started("pack-a", blobs, token);
  await put(hash("a"), 8);
  await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, token);
  expect((await api("/packs/pack-a", { method: "DELETE" }, token)).status).toBe(200);
  const res = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, token);
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("publish_gone");
  expect((await begin("pack-b", [{ sha256: hash("b"), size: 8 }], token)).status).toBe(507);
});

it("a commit in flight when its pack is deleted writes nothing", async () => {
  const token = await signIn("u1");
  const blobs = [{ sha256: hash("a"), size: 5 }];
  const { publish_id } = await started("pack-a", blobs, token);
  await put(hash("a"), 5);
  await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, token);

  const real = env.DB;
  const wrapped = new Proxy(real, {
    get(target, prop) {
      if (prop === "batch") {
        return async (stmts: D1PreparedStatement[]) => {
          await real.prepare("delete from packs where id = 'pack-a'").run();
          return real.batch(stmts);
        };
      }
      const v = Reflect.get(target, prop, target);
      return typeof v === "function" ? v.bind(target) : v;
    },
  });
  (env as unknown as Record<string, unknown>).DB = wrapped;
  let res: Response;
  try {
    res = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-a", blobs) }, token);
  } finally {
    (env as unknown as Record<string, unknown>).DB = real;
  }
  expect(res.status).toBe(404);
  expect(((await res.json()) as { error: string }).error).toBe("publish_gone");
  for (const t of ["blobs", "pack_blobs", "packs"]) {
    const n = await env.DB.prepare(`select count(*) as n from ${t}`).first<{ n: number }>();
    expect(n!.n).toBe(0);
  }
  const held = await env.DB.prepare("select count(*) as n from publish_blobs").first<{ n: number }>();
  expect(held!.n).toBe(1);
});

it("a body that is not a JSON object is a bad request", async () => {
  const token = await signIn("u1");
  for (const body of ["null", "5", "[]"]) {
    const res = await api("/packs/pack-a/publish/begin", { method: "POST", body }, token);
    expect(res.status).toBe(400);
    expect(((await res.json()) as { error: string }).error).toBe("bad_request");
  }
});

it("begin refuses a blob too large for one upload", async () => {
  const token = await signIn("u1");
  const res = await begin("pack-a", [{ sha256: hash("a"), size: 5 * 1024 ** 3 + 1 }], token);
  expect(res.status).toBe(400);
  expect((await begin("pack-a", [{ sha256: hash("a"), size: 5 * 1024 ** 3 }], token)).status).toBe(200);
});

const beginAs = (pack: string, token: string, base: string | null, version = "2.0") =>
  post(`/packs/${pack}/publish/begin`, { name: "Pack", version, blobs: [], base }, token);

const emptyManifest = (pack: string, version: string) => manifest(pack, [], version);

const commitOf = (id: string, pack: string, version: string, token: string) =>
  post(`/publishes/${id}/commit`, { manifest: emptyManifest(pack, version) }, token);

async function publishedPack(version = "1.0.0") {
  const creator = await signIn("c1", "Cora");
  const editor = await signIn("e1", "Eddie", null);
  await env.DB.prepare(
    "insert into packs (id, uploader_id, name, version, manifest, published_at) values ('pack-h', 'c1', 'Pack', ?, '{}', ?)",
  )
    .bind(version, nowSeconds())
    .run();
  await env.DB.prepare("insert into pack_editors (pack_id, user_id) values ('pack-h', 'e1')").run();
  return { creator, editor };
}

const errCode = async (res: Response) => ((await res.json()) as { error: string }).error;

it("an editor can publish to the pack", async () => {
  const { editor } = await publishedPack();
  const res = await beginAs("pack-h", editor, "1.0.0", "1.1.0");
  expect(res.status).toBe(200);
  const { publish_id } = (await res.json()) as { publish_id: string };
  expect((await commitOf(publish_id, "pack-h", "1.1.0", editor)).status).toBe(200);
  const row = await env.DB.prepare("select version from packs where id = 'pack-h'").first<{ version: string }>();
  expect(row!.version).toBe("1.1.0");
});

it("begin refuses a stale base", async () => {
  const { editor } = await publishedPack("1.1.0");
  const res = await beginAs("pack-h", editor, "1.0.0");
  expect(res.status).toBe(409);
  expect(await errCode(res)).toBe("pack_behind");
});

it("begin refuses a missing base for a published pack", async () => {
  const { creator } = await publishedPack();
  const res = await beginAs("pack-h", creator, null);
  expect(res.status).toBe(409);
  expect(await errCode(res)).toBe("pack_behind");
  const first = await signIn("u9");
  expect((await beginAs("fresh", first, null, "1.0.0")).status).toBe(200);
});

it("commit refuses a publish whose base moved", async () => {
  const { editor } = await publishedPack("1.0.0");
  const blobs = [{ sha256: hash("a"), size: 3 }];
  const res = await post(
    "/packs/pack-h/publish/begin",
    { name: "Pack", version: "2.0.0", blobs, base: "1.0.0" },
    editor,
  );
  const { publish_id } = (await res.json()) as { publish_id: string };
  await put(hash("a"), 3);
  await post(`/publishes/${publish_id}/verify`, { blobs: [hash("a")] }, editor);
  await env.DB.prepare("update packs set version = '1.1.0' where id = 'pack-h'").run();
  const c = await post(`/publishes/${publish_id}/commit`, { manifest: manifest("pack-h", blobs, "2.0.0") }, editor);
  expect(c.status).toBe(409);
  expect(await errCode(c)).toBe("pack_behind");
  const row = await env.DB.prepare("select version from packs where id = 'pack-h'").first<{ version: string }>();
  expect(row!.version).toBe("1.1.0");
  const n = await env.DB.prepare("select count(*) as n from pack_blobs").first<{ n: number }>();
  expect(n!.n).toBe(0);
});

it("a second person is refused while a publish is open", async () => {
  const { creator, editor } = await publishedPack();
  expect((await beginAs("pack-h", editor, "1.0.0")).status).toBe(200);
  const res = await beginAs("pack-h", creator, "1.0.0");
  expect(res.status).toBe(409);
  const body = (await res.json()) as { error: string; message: string };
  expect(body.error).toBe("pack_busy");
  expect(body.message).toContain("Eddie");
});

it("same user begin takes over their own open publish", async () => {
  const { editor } = await publishedPack();
  const a = (await (await beginAs("pack-h", editor, "1.0.0")).json()) as { publish_id: string };
  const b = await beginAs("pack-h", editor, "1.0.0");
  expect(b.status).toBe(200);
  const c = await commitOf(a.publish_id, "pack-h", "2.0", editor);
  expect(c.status).toBe(404);
  expect(await errCode(c)).toBe("publish_gone");
});

it("the hold ends when the publish expires", async () => {
  const { creator, editor } = await publishedPack();
  await beginAs("pack-h", editor, "1.0.0");
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() - 1).run();
  expect((await beginAs("pack-h", creator, "1.0.0")).status).toBe(200);
});

it("urls and verify push the expiry back", async () => {
  const { creator, editor } = await publishedPack();
  const start = nowSeconds();
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(start * 1000);
  const { publish_id } = (await (await beginAs("pack-h", editor, "1.0.0")).json()) as { publish_id: string };
  vi.setSystemTime((start + 3000) * 1000);
  expect((await post(`/publishes/${publish_id}/urls`, { blobs: [] }, editor)).status).toBe(200);
  vi.setSystemTime((start + 3700) * 1000);
  expect((await beginAs("pack-h", creator, "1.0.0")).status).toBe(409);
  vi.setSystemTime((start + 6000) * 1000);
  expect((await post(`/publishes/${publish_id}/verify`, { blobs: [] }, editor)).status).toBe(200);
  vi.setSystemTime((start + 9000) * 1000);
  expect((await beginAs("pack-h", creator, "1.0.0")).status).toBe(409);
});

it("clearing the hold frees the pack", async () => {
  const { creator, editor } = await publishedPack();
  const { publish_id } = (await (await beginAs("pack-h", editor, "1.0.0")).json()) as { publish_id: string };
  expect((await api("/packs/pack-h/hold", { method: "DELETE" }, editor)).status).toBe(403);
  const res = await api("/packs/pack-h/hold", { method: "DELETE" }, creator);
  expect(res.status).toBe(200);
  expect(await res.json()).toEqual({ cleared: 1 });
  expect((await beginAs("pack-h", creator, "1.0.0")).status).toBe(200);
  const commit = await commitOf(publish_id, "pack-h", "2.0", editor);
  expect(commit.status).toBe(409);
  expect(await errCode(commit)).toBe("publish_cleared");
  for (const step of ["urls", "verify"]) {
    const res = await post(`/publishes/${publish_id}/${step}`, { blobs: [] }, editor);
    expect(res.status).toBe(409);
    expect(await errCode(res)).toBe("publish_cleared");
  }
});

it("an expired publish is gone, not cleared", async () => {
  const { editor } = await publishedPack();
  const { publish_id } = (await (await beginAs("pack-h", editor, "1.0.0")).json()) as { publish_id: string };
  await env.DB.prepare("update publishes set expires_at = ?").bind(nowSeconds() - 1).run();
  const res = await post(`/publishes/${publish_id}/urls`, { blobs: [] }, editor);
  expect(res.status).toBe(404);
  expect(await errCode(res)).toBe("publish_gone");
});
