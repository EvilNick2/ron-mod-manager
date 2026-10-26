import { nowSeconds, randomToken, requireUser, type User } from "./auth";
import { numberVar, type Env } from "./env";
import { cancelledExpiry } from "./cleanup";
import { HttpError, json, readJson } from "./http";
import { presignPut } from "./presign";
import { DB_BYTES_SQL, classAUsed, countClassA, extraBucketBytes } from "./storage";

const PUBLISH_LIFETIME = 3600;
const MAX_BATCH = 50;
const MAX_BLOBS = 2000;
// R2 refuses a single PUT larger than 5 GiB.
const MAX_BLOB_SIZE = 5 * 1024 ** 3;
const PACK_ID = /^[A-Za-z0-9-]{1,64}$/;
const HASH = /^[0-9a-f]{64}$/;

interface BlobRef {
  sha256: string;
  size: number;
}

interface PublishRow {
  id: string;
  pack_id: string;
  user_id: string;
  name: string;
  version: string;
}

const badRequest = (msg: string) => new HttpError(400, "bad_request", msg);
const gone = () => new HttpError(404, "publish_gone", "This publish is no longer available");

async function requireUploader(req: Request, env: Env): Promise<User> {
  const user = await requireUser(req, env);
  if (!user.role) throw new HttpError(403, "forbidden", "Only uploaders can publish");
  return user;
}

async function ownPublish(env: Env, user: User, id: string): Promise<PublishRow> {
  const row = await env.DB.prepare(
    "select id, pack_id, user_id, name, version from publishes where id = ? and user_id = ? and expires_at > ?",
  )
    .bind(id, user.id, nowSeconds())
    .first<PublishRow>();
  if (!row) throw gone();
  return row;
}

function hashList(value: unknown): string[] {
  if (!Array.isArray(value) || value.length > MAX_BATCH) {
    throw badRequest(`Send at most ${MAX_BATCH} blobs`);
  }
  for (const h of value) if (typeof h !== "string" || !HASH.test(h)) throw badRequest("Bad blob hash");
  return value as string[];
}

async function declared(env: Env, publishId: string): Promise<Map<string, number>> {
  const rows = await env.DB.prepare("select sha256, size from publish_blobs where publish_id = ?")
    .bind(publishId)
    .all<BlobRef>();
  return new Map(rows.results.map((r) => [r.sha256, r.size]));
}

function inPublish(known: Map<string, number>, hashes: string[]): void {
  for (const h of hashes) if (!known.has(h)) throw badRequest("Blob is not part of this publish");
}

export async function beginPublish(req: Request, env: Env, packId: string): Promise<Response> {
  const user = await requireUploader(req, env);
  const body = await readJson<{ name?: unknown; version?: unknown; blobs?: unknown }>(req);
  if (!PACK_ID.test(packId) || /^[0-9]+$/.test(packId)) throw badRequest("Bad pack id");
  for (const v of [body.name, body.version]) {
    if (typeof v !== "string" || v.length < 1 || v.length > 200) {
      throw badRequest("Name and version must be 1 to 200 characters");
    }
  }
  if (!Array.isArray(body.blobs) || body.blobs.length > MAX_BLOBS) {
    throw badRequest(`Blobs must be a list of at most ${MAX_BLOBS}`);
  }
  const wanted = new Map<string, number>();
  for (const b of body.blobs as BlobRef[]) {
    if (!b || typeof b.sha256 !== "string" || !HASH.test(b.sha256)) throw badRequest("Bad blob hash");
    if (!Number.isSafeInteger(b.size) || b.size < 0) throw badRequest("Bad blob size");
    if (b.size > MAX_BLOB_SIZE) throw badRequest("A file is larger than 5 GiB and cannot be uploaded");
    if (wanted.has(b.sha256) && wanted.get(b.sha256) !== b.size) throw badRequest("Conflicting sizes");
    wanted.set(b.sha256, b.size);
  }

  const pack = await env.DB.prepare("select uploader_id from packs where id = ?")
    .bind(packId)
    .first<{ uploader_id: string }>();
  if (pack && pack.uploader_id !== user.id) {
    throw new HttpError(409, "pack_taken", "Another uploader owns this pack id");
  }

  const hashes = [...wanted.keys()];
  const listJson = JSON.stringify(hashes);
  const stored = await env.DB.prepare(
    "select sha256, size from blobs where sha256 in (select value from json_each(?))",
  )
    .bind(listJson)
    .all<BlobRef>();
  const held = await env.DB.prepare(
    "select sha256, size from publish_blobs where sha256 in (select value from json_each(?))",
  )
    .bind(listJson)
    .all<BlobRef>();
  for (const r of [...stored.results, ...held.results]) {
    if (wanted.get(r.sha256) !== r.size) throw badRequest("Declared size differs from a known blob");
  }
  const have = new Set(stored.results.map((r) => r.sha256));
  const reserved = new Set(held.results.map((r) => r.sha256));
  const missing = hashes.filter((h) => !have.has(h));
  const newBytes = missing.filter((h) => !reserved.has(h)).reduce((n, h) => n + wanted.get(h)!, 0);

  const cap = numberVar(env, "STORAGE_CAP_BYTES");
  const budget = numberVar(env, "OP_BUDGET");
  const extra = await extraBucketBytes(env);
  if ((await classAUsed(env)) + missing.length > budget) {
    throw new HttpError(429, "op_budget", "The monthly upload budget is used up");
  }

  const publishId = randomToken();
  const rows = JSON.stringify(hashes.map((h) => ({ h, s: wanted.get(h)! })));
  const live = "exists (select 1 from publishes where id = ?)";
  const results = await env.DB.batch([
    env.DB.prepare(
      `insert into publishes (id, pack_id, user_id, name, version, expires_at)
       select ?, ?, ?, ?, ?, ? where ${DB_BYTES_SQL} + ? <= ?`,
    ).bind(
      publishId, packId, user.id, body.name, body.version,
      nowSeconds() + PUBLISH_LIFETIME, extra + newBytes, cap,
    ),
    env.DB.prepare(
      `insert or ignore into packs (id, uploader_id, name, version)
       select ?, ?, ?, ? where ${live}`,
    ).bind(packId, user.id, body.name, body.version, publishId),
    env.DB.prepare(
      `insert into publish_blobs (publish_id, sha256, size)
       select ?, json_extract(value, '$.h'), json_extract(value, '$.s')
       from json_each(?) where ${live}`,
    ).bind(publishId, rows, publishId),
  ]);
  if (results[0].meta.changes !== 1) {
    throw new HttpError(507, "storage_full", "The server has no room for this pack");
  }
  const owner = await env.DB.prepare("select uploader_id from packs where id = ?")
    .bind(packId)
    .first<{ uploader_id: string }>();
  if (owner?.uploader_id !== user.id) {
    await dropPublish(env, publishId);
    throw new HttpError(409, "pack_taken", "Another uploader owns this pack id");
  }
  return json({ publish_id: publishId, missing });
}

async function dropPublish(env: Env, id: string): Promise<void> {
  await env.DB.batch([
    env.DB.prepare("delete from publish_blobs where publish_id = ?").bind(id),
    env.DB.prepare("delete from publishes where id = ?").bind(id),
  ]);
}

async function storedHashes(env: Env, hashes: string[]): Promise<Set<string>> {
  if (!hashes.length) return new Set();
  const rows = await env.DB.prepare(
    "select sha256 from blobs where sha256 in (select value from json_each(?))",
  )
    .bind(JSON.stringify(hashes))
    .all<{ sha256: string }>();
  return new Set(rows.results.map((r) => r.sha256));
}

export async function publishUrls(req: Request, env: Env, pid: string): Promise<Response> {
  const user = await requireUploader(req, env);
  const publish = await ownPublish(env, user, pid);
  const body = await readJson<{ blobs?: unknown }>(req);
  const hashes = hashList(body.blobs);
  const known = await declared(env, publish.id);
  inPublish(known, hashes);
  if ((await storedHashes(env, hashes)).size) {
    throw new HttpError(400, "already_stored", "A blob is already stored, verify it instead");
  }
  const budget = numberVar(env, "OP_BUDGET");
  if ((await classAUsed(env)) + hashes.length > budget) {
    throw new HttpError(429, "op_budget", "The monthly upload budget is used up");
  }
  await countClassA(env, hashes.length);
  const urls: Record<string, string> = {};
  for (const h of hashes) urls[h] = await presignPut(env, h, known.get(h)!);
  return json({ urls });
}

export async function verifyPublish(req: Request, env: Env, pid: string): Promise<Response> {
  const user = await requireUploader(req, env);
  const publish = await ownPublish(env, user, pid);
  const body = await readJson<{ blobs?: unknown }>(req);
  const hashes = hashList(body.blobs);
  const known = await declared(env, publish.id);
  inPublish(known, hashes);
  const stored = await storedHashes(env, hashes);
  const verified: string[] = [];
  const bad: string[] = [];
  for (const h of hashes) {
    if (stored.has(h)) {
      verified.push(h);
      continue;
    }
    const head = await env.PACKS.head(`blobs/${h}`);
    if (head && head.size === known.get(h)) {
      verified.push(h);
    } else {
      if (head) await env.PACKS.delete(`blobs/${h}`);
      bad.push(h);
    }
  }
  if (verified.length) {
    await env.DB.prepare(
      `update publish_blobs set verified = 1
       where publish_id = ? and sha256 in (select value from json_each(?))`,
    )
      .bind(publish.id, JSON.stringify(verified))
      .run();
  }
  return json({ verified, bad });
}

function manifestPairs(manifest: any, packId: string): Set<string> {
  const bad = () => new HttpError(400, "bad_manifest", "The manifest does not match this publish");
  if (!manifest || manifest.format !== 2 || manifest.pack_id !== packId || !Array.isArray(manifest.mods)) {
    throw bad();
  }
  const pairs = new Set<string>();
  for (const mod of manifest.mods) {
    if (!mod || !Array.isArray(mod.files)) throw bad();
    for (const f of mod.files) {
      if (!f || typeof f.sha256 !== "string" || !Number.isSafeInteger(f.size)) throw bad();
      pairs.add(`${f.sha256}:${f.size}`);
    }
  }
  return pairs;
}

export async function commitPublish(req: Request, env: Env, pid: string): Promise<Response> {
  const user = await requireUploader(req, env);
  const publish = await ownPublish(env, user, pid);
  const body = await readJson<{ manifest?: any }>(req);
  const known = await declared(env, publish.id);
  const want = new Set([...known].map(([h, s]) => `${h}:${s}`));
  const got = manifestPairs(body.manifest, publish.pack_id);
  if (got.size !== want.size || [...got].some((p) => !want.has(p))) {
    throw new HttpError(400, "bad_manifest", "The manifest does not match this publish");
  }

  const unverified = await env.DB.prepare(
    `select count(*) as n from publish_blobs
     where publish_id = ? and verified = 0 and sha256 not in (select sha256 from blobs)`,
  )
    .bind(publish.id)
    .first<{ n: number }>();
  if (unverified && unverified.n > 0) {
    throw new HttpError(409, "not_uploaded", "Some blobs have not been uploaded and verified");
  }

  const owner = await env.DB.prepare("select uploader_id from packs where id = ?")
    .bind(publish.pack_id)
    .first<{ uploader_id: string }>();
  if (owner?.uploader_id !== user.id) {
    throw new HttpError(409, "pack_taken", "Another uploader owns this pack id");
  }

  const publishedAt = nowSeconds();
  const stored = JSON.stringify({
    ...body.manifest,
    name: publish.name,
    version: publish.version,
    published_at: publishedAt,
  });
  const rows = JSON.stringify([...known].map(([h, size]) => ({ h, s: size })));
  const live = `exists (select 1 from publishes where id = ? and expires_at > ?)
    and exists (select 1 from packs where id = ? and uploader_id = ?)`;
  const guard = [publish.id, publishedAt, publish.pack_id, user.id];
  const stmts = [
    env.DB.prepare(
      `insert or ignore into blobs (sha256, size)
       select json_extract(value, '$.h'), json_extract(value, '$.s') from json_each(?)
       where ${live}`,
    ).bind(rows, ...guard),
    env.DB.prepare(`delete from pack_blobs where pack_id = ? and ${live}`).bind(publish.pack_id, ...guard),
    env.DB.prepare(
      `insert into pack_blobs (pack_id, sha256)
       select ?, json_extract(value, '$.h') from json_each(?) where ${live}`,
    ).bind(publish.pack_id, rows, ...guard),
    env.DB.prepare(
      `update packs set manifest = ?, name = ?, version = ?, published_at = ?
       where id = ? and uploader_id = ? and ${live}`,
    ).bind(stored, publish.name, publish.version, publishedAt, publish.pack_id, user.id, ...guard),
    env.DB.prepare(`delete from publish_blobs where publish_id = ? and ${live}`).bind(publish.id, ...guard),
    env.DB.prepare(`delete from publishes where id = ? and ${live}`).bind(publish.id, ...guard),
  ];
  const results = await env.DB.batch(stmts);
  if (results[3].meta.changes !== 1 || results[5].meta.changes !== 1) throw gone();
  return json({ ok: true });
}

export async function cancelPublish(req: Request, env: Env, pid: string): Promise<Response> {
  const user = await requireUploader(req, env);
  const publish = await ownPublish(env, user, pid);
  await env.DB.prepare("update publishes set expires_at = ? where id = ?")
    .bind(cancelledExpiry(nowSeconds()), publish.id)
    .run();
  return json({ ok: true });
}
