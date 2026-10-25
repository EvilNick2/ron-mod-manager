import { env } from "cloudflare:workers";
import { afterEach, beforeEach, expect, it } from "vitest";
import { nowSeconds } from "../src/auth";
import { api, signIn } from "./helpers";

const realCap = env.STORAGE_CAP_BYTES;
const realBudget = env.OP_BUDGET;
const setVar = (name: "STORAGE_CAP_BYTES" | "OP_BUDGET", v: string) => {
  (env as unknown as Record<string, string>)[name] = v;
};

beforeEach(async () => {
  for (const t of [
    "users", "sessions", "packs", "blobs", "pack_blobs", "publishes", "publish_blobs", "op_counts",
  ]) {
    await env.DB.prepare(`delete from ${t}`).run();
  }
  for (const b of [env.PACKS, env.EXTRA_BUCKET!]) {
    const list = await b.list();
    for (const o of list.objects) await b.delete(o.key);
  }
});

afterEach(() => {
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
) => post(`/packs/${pack}/publish/begin`, { name: "Pack", version, blobs }, token);

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
  expect(await c1.json()).toEqual({ ok: true });

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

it("another user sees a publish as gone and cancel frees it", async () => {
  const t1 = await signIn("u1");
  const t2 = await signIn("u2");
  const { publish_id } = await started("pack-a", [{ sha256: hash("a"), size: 5 }], t1);
  expect((await api(`/publishes/${publish_id}`, { method: "DELETE" }, t2)).status).toBe(404);
  expect((await api(`/publishes/${publish_id}`, { method: "DELETE" }, t1)).status).toBe(200);
  const left = await env.DB.prepare("select count(*) as n from publish_blobs").first<{ n: number }>();
  expect(left?.n).toBe(0);
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
