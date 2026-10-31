import { nowSeconds } from "../auth";
import { cleanup } from "../cleanup";
import { numberVar, type Env } from "../env";
import { json } from "../http";
import { mintUploaderInvite } from "../invites";
import {
  clearPackHold,
  deletePack,
  listUserRows,
  loadPack,
  refuseSelfRemoval,
  removeUserAccount,
} from "../manage";
import { DB_BYTES_SQL, classAUsed, takeSample } from "../storage";
import { requireAdmin } from "./session";

const OPEN_HASHES = `select pb.sha256 from publish_blobs pb
  join publishes p on p.id = pb.publish_id where p.expires_at > ?`;

async function leftovers(env: Env, now: number): Promise<{ files: number; bytes: number }> {
  const row = await env.DB.prepare(
    `select count(*) as files, coalesce(sum(size), 0) as bytes from (
       select sha256, size from blobs b
       where not exists (select 1 from pack_blobs where sha256 = b.sha256)
         and sha256 not in (${OPEN_HASHES})
       union
       select pb.sha256, max(pb.size) as size from publish_blobs pb
       join publishes p on p.id = pb.publish_id
       where p.expires_at <= ?
         and pb.sha256 not in (select sha256 from blobs)
         and pb.sha256 not in (${OPEN_HASHES})
       group by pb.sha256
     )`,
  )
    .bind(now, now, now)
    .first<{ files: number; bytes: number }>();
  return { files: row?.files ?? 0, bytes: row?.bytes ?? 0 };
}

const newestSample = (env: Env) =>
  env.DB.prepare("select extra_bytes from storage_samples order by taken_at desc limit 1").first<{
    extra_bytes: number;
  }>();

export async function adminOverview(req: Request, env: Env): Promise<Response> {
  await requireAdmin(req, env);
  const now = nowSeconds();

  let sample = await newestSample(env);
  if (!sample) {
    await takeSample(env, now);
    sample = await newestSample(env);
  }

  const sizes = await env.DB.prepare(
    `select coalesce(sum(case when n = 1 then size end), 0) as single,
            coalesce(sum(case when n > 1 then size end), 0) as shared,
            ${DB_BYTES_SQL} as total
     from (select b.size, (select count(*) from pack_blobs pb where pb.sha256 = b.sha256) as n from blobs b)`,
  ).first<{ single: number; shared: number; total: number }>();
  const single = sizes?.single ?? 0;
  const shared = sizes?.shared ?? 0;
  const waiting = (sizes?.total ?? 0) - single - shared;
  const bound = Boolean(env.EXTRA_BUCKET);
  const extra = bound ? (sample?.extra_bytes ?? 0) : null;

  const samples = await env.DB.prepare("select taken_at as t, bytes from storage_samples order by taken_at").all<{
    t: number;
    bytes: number;
  }>();
  const publishes = await env.DB.prepare(
    `select p.pack_id, p.name, p.version, u.name as user, p.expires_at
     from publishes p left join users u on u.id = p.user_id
     where p.expires_at > ? order by p.expires_at`,
  )
    .bind(now)
    .all();

  const d = new Date(now * 1000);
  return json({
    storage: {
      cap: numberVar(env, "STORAGE_CAP_BYTES"),
      used: single + shared + waiting + (extra ?? 0),
      single,
      shared,
      waiting,
      extra,
      extra_label: bound ? (env.EXTRA_BUCKET_LABEL || "Other bucket") : null,
    },
    samples: samples.results,
    ops: {
      used: await classAUsed(env),
      budget: numberVar(env, "OP_BUDGET"),
      resets_at: Date.UTC(d.getUTCFullYear(), d.getUTCMonth() + 1, 1) / 1000,
    },
    publishes: publishes.results,
    leftovers: await leftovers(env, now),
  });
}

export async function adminPacks(req: Request, env: Env): Promise<Response> {
  await requireAdmin(req, env);
  const packs = await env.DB.prepare(
    `select p.id, p.name, p.version, p.published_at, p.last_used_at, p.last_downloaded_at, p.download_count,
            coalesce(sum(case when r.n = 1 then b.size end), 0) as frees,
            coalesce(sum(case when r.n > 1 then b.size end), 0) as shared_bytes,
            (select count(*) from pack_viewers v where v.pack_id = p.id) as viewers,
            (select count(*) from pack_editors e where e.pack_id = p.id) as editors
     from packs p
     left join pack_blobs pb on pb.pack_id = p.id
     left join blobs b on b.sha256 = pb.sha256
     left join (select sha256, count(*) as n from pack_blobs group by sha256) r on r.sha256 = pb.sha256
     group by p.id
     order by p.name, p.id`,
  ).all<{ id: string }>();
  const links = await env.DB.prepare(
    `select distinct a.pack_id as id, o.name
     from pack_blobs a join pack_blobs c on c.sha256 = a.sha256 and c.pack_id <> a.pack_id
     join packs o on o.id = c.pack_id order by o.name, o.id`,
  ).all<{ id: string; name: string }>();
  const withNames = new Map<string, string[]>();
  for (const l of links.results) withNames.set(l.id, [...(withNames.get(l.id) ?? []), l.name]);
  const started = await env.DB.prepare("select started_at from tracking where id = 1").first<{
    started_at: number;
  }>();
  return json({
    tracking_started: started?.started_at ?? null,
    packs: packs.results.map((p) => ({ ...p, shared_with: withNames.get(p.id) ?? [] })),
  });
}

export async function adminPackDetail(req: Request, env: Env, id: string): Promise<Response> {
  await requireAdmin(req, env);
  const pack = await loadPack(env, id);
  const names = (sql: string) =>
    env.DB.prepare(sql)
      .bind(pack.id)
      .all<{ name: string }>()
      .then((r) => r.results.map((x) => x.name));
  const creator = await env.DB.prepare("select name from users where id = ?")
    .bind(pack.uploader_id)
    .first<{ name: string }>();
  const versions = await env.DB.prepare(
    "select version, published_at, publisher from pack_versions where pack_id = ? order by id desc",
  )
    .bind(pack.id)
    .all();
  return json({
    creator: creator?.name ?? null,
    versions: versions.results,
    editors: await names(
      "select u.name from pack_editors e join users u on u.id = e.user_id where e.pack_id = ? order by u.name, u.id",
    ),
    viewers: await names(
      "select u.name from pack_viewers v join users u on u.id = v.user_id where v.pack_id = ? order by u.name, u.id",
    ),
  });
}

export async function adminDeletePack(req: Request, env: Env, id: string): Promise<Response> {
  await requireAdmin(req, env);
  await deletePack(env, (await loadPack(env, id)).id);
  return json({ ok: true });
}

export async function adminClearHold(req: Request, env: Env, id: string): Promise<Response> {
  await requireAdmin(req, env);
  const pack = await loadPack(env, id);
  return json({ cleared: await clearPackHold(env, pack.id) });
}

export async function adminPeople(req: Request, env: Env): Promise<Response> {
  await requireAdmin(req, env);
  const users = await listUserRows(env);
  const created = await env.DB.prepare("select uploader_id, name from packs order by name, id").all<{
    uploader_id: string;
    name: string;
  }>();
  const counts = async (table: string) => {
    const r = await env.DB.prepare(`select user_id, count(*) as n from ${table} group by user_id`).all<{
      user_id: string;
      n: number;
    }>();
    return new Map(r.results.map((x) => [x.user_id, x.n]));
  };
  const edits = await counts("pack_editors");
  const views = await counts("pack_viewers");
  return json({
    max_users: numberVar(env, "MAX_USERS"),
    people: users.map((u) => ({
      id: u.id,
      name: u.name,
      role: u.role,
      created: created.results.filter((p) => p.uploader_id === u.id).map((p) => p.name),
      edits: edits.get(u.id) ?? 0,
      views: views.get(u.id) ?? 0,
    })),
  });
}

export async function adminDeleteUser(req: Request, env: Env, id: string): Promise<Response> {
  const owner = await requireAdmin(req, env);
  refuseSelfRemoval(owner.id, id);
  await removeUserAccount(env, id);
  return json({ ok: true });
}

export async function adminInviteUploader(req: Request, env: Env): Promise<Response> {
  const owner = await requireAdmin(req, env);
  return json(await mintUploaderInvite(env, owner.id));
}

export async function adminCleanup(req: Request, env: Env): Promise<Response> {
  await requireAdmin(req, env);
  const now = nowSeconds();
  await cleanup(env, now);
  return json(await leftovers(env, now));
}
