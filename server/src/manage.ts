import { DAY, nowSeconds, randomToken, requireUser, type User } from "./auth";
import { cancelledExpiry } from "./cleanup";
import type { Env } from "./env";
import { HttpError, json } from "./http";
import { inviteUrl } from "./invites";

const PACK_ID = /^[A-Za-z0-9-]{1,64}$/;

const packGone = () => new HttpError(404, "pack_gone", "This pack is not available");
const userGone = () => new HttpError(404, "user_gone", "This person is not on the server");

// `ids` is SQL giving pack ids from one bound value. Open publishes are expired, not deleted, so their reservations count until cleanup.
function packStatements(env: Env, ids: string, arg: string): D1PreparedStatement[] {
  const run = (sql: string) => env.DB.prepare(sql).bind(arg);
  return [
    run(`delete from pack_viewers where pack_id in (${ids})`),
    run(`delete from pack_blobs where pack_id in (${ids})`),
    run(`delete from invites where pack_id in (${ids})`),
    env.DB.prepare(`update publishes set expires_at = min(expires_at, ?) where pack_id in (${ids})`).bind(
      cancelledExpiry(nowSeconds()),
      arg,
    ),
    run(`delete from packs where id in (${ids})`),
  ];
}

export async function deletePack(env: Env, packId: string): Promise<void> {
  await env.DB.batch(packStatements(env, "?", packId));
}

async function loadPack(env: Env, id: string): Promise<{ id: string; uploader_id: string }> {
  if (!PACK_ID.test(id) || /^[0-9]+$/.test(id)) throw packGone();
  const pack = await env.DB.prepare("select id, uploader_id from packs where id = ?")
    .bind(id)
    .first<{ id: string; uploader_id: string }>();
  if (!pack) throw packGone();
  return pack;
}

async function managedPack(req: Request, env: Env, id: string): Promise<{ user: User; packId: string }> {
  const user = await requireUser(req, env);
  const pack = await loadPack(env, id);
  if (user.role !== "owner" && pack.uploader_id !== user.id) {
    throw new HttpError(403, "forbidden", "Only the pack's uploader can do this");
  }
  return { user, packId: pack.id };
}

async function mintViewerInvite(env: Env, packId: string, userId: string) {
  const code = randomToken();
  const expiresAt = nowSeconds() + DAY;
  await env.DB.prepare(
    "insert into invites (code, kind, pack_id, created_by, expires_at) values (?, 'viewer', ?, ?, ?)",
  )
    .bind(code, packId, userId, expiresAt)
    .run();
  return json({ url: inviteUrl(env, code), expires_at: expiresAt });
}

export async function packInvite(req: Request, env: Env, id: string): Promise<Response> {
  const { user, packId } = await managedPack(req, env, id);
  const existing = await env.DB.prepare(
    `select code, expires_at from invites
     where kind = 'viewer' and pack_id = ? and used_at is null and expires_at > ?
     order by expires_at desc limit 1`,
  )
    .bind(packId, nowSeconds())
    .first<{ code: string; expires_at: number }>();
  if (existing) return json({ url: inviteUrl(env, existing.code), expires_at: existing.expires_at });
  return mintViewerInvite(env, packId, user.id);
}

export async function resetPackInvite(req: Request, env: Env, id: string): Promise<Response> {
  const { user, packId } = await managedPack(req, env, id);
  await env.DB.prepare("delete from invites where kind = 'viewer' and pack_id = ?").bind(packId).run();
  return mintViewerInvite(env, packId, user.id);
}

export async function listViewers(req: Request, env: Env, id: string): Promise<Response> {
  const { packId } = await managedPack(req, env, id);
  const rows = await env.DB.prepare(
    `select u.id, u.name from pack_viewers v join users u on u.id = v.user_id
     where v.pack_id = ? order by u.name, u.id`,
  )
    .bind(packId)
    .all<{ id: string; name: string }>();
  return json(rows.results);
}

export async function removeViewer(req: Request, env: Env, id: string, userId: string): Promise<Response> {
  const { packId } = await managedPack(req, env, id);
  const [invites, res] = await env.DB.batch([
    env.DB.prepare(
      `delete from invites where kind = 'viewer' and pack_id = ?
       and exists (select 1 from pack_viewers where pack_id = ? and user_id = ?)`,
    ).bind(packId, packId, userId),
    env.DB.prepare("delete from pack_viewers where pack_id = ? and user_id = ?").bind(packId, userId),
  ]);
  if (res.meta.changes !== 1) throw userGone();
  return json({ ok: true, invite_reset: invites.meta.changes > 0 });
}

export async function removePack(req: Request, env: Env, id: string): Promise<Response> {
  const { packId } = await managedPack(req, env, id);
  await deletePack(env, packId);
  return json({ ok: true });
}

export async function leavePack(req: Request, env: Env, id: string): Promise<Response> {
  const user = await requireUser(req, env);
  const pack = await loadPack(env, id);
  if (pack.uploader_id === user.id) {
    throw new HttpError(400, "own_pack", "You cannot leave your own pack");
  }
  const res = await env.DB.prepare("delete from pack_viewers where pack_id = ? and user_id = ?")
    .bind(pack.id, user.id)
    .run();
  if (res.meta.changes !== 1) throw packGone();
  return json({ ok: true });
}

async function requireOwner(req: Request, env: Env): Promise<User> {
  const user = await requireUser(req, env);
  if (user.role !== "owner") throw new HttpError(403, "forbidden", "Only the owner can do this");
  return user;
}

export async function listUsers(req: Request, env: Env): Promise<Response> {
  await requireOwner(req, env);
  const rows = await env.DB.prepare(
    `select u.id, u.name, u.role,
            (select count(*) from packs p where p.uploader_id = u.id) as packs
     from users u
     where u.role is not null
        or exists (select 1 from pack_viewers v where v.user_id = u.id)
     order by u.name, u.id`,
  ).all<{ id: string; name: string; role: User["role"]; packs: number }>();
  return json(rows.results);
}

export async function removeUser(req: Request, env: Env, id: string): Promise<Response> {
  const owner = await requireOwner(req, env);
  if (id === owner.id) throw new HttpError(400, "self_remove", "The owner cannot remove themselves");
  const target = await env.DB.prepare("select id from users where id = ?").bind(id).first();
  if (!target) throw userGone();
  const run = (sql: string) => env.DB.prepare(sql).bind(id);
  await env.DB.batch([
    ...packStatements(env, "select id from packs where uploader_id = ?", id),
    run(`delete from invites where kind = 'viewer'
         and pack_id in (select pack_id from pack_viewers where user_id = ?)`),
    run("delete from pack_viewers where user_id = ?"),
    run("delete from sessions where user_id = ?"),
    run("delete from invites where created_by = ?"),
    env.DB.prepare("update publishes set expires_at = min(expires_at, ?) where user_id = ?").bind(
      cancelledExpiry(nowSeconds()),
      id,
    ),
    run("update users set role = null where id = ?"),
  ]);
  return json({ ok: true });
}
