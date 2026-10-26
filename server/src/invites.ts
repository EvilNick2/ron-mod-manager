import { DAY, nowSeconds, randomToken, requireUser, type User } from "./auth";
import { numberVar, type Env } from "./env";
import { HttpError, json } from "./http";

interface InviteRow {
  code: string;
  kind: "uploader" | "viewer";
  pack_id: string | null;
  expires_at: number;
}

interface PackRow {
  id: string;
  name: string;
  version: string;
  uploader_id: string;
  uploader_name: string;
}

const CODE_PATTERN = /^[A-Za-z0-9_-]+$/;

const gone = () => new HttpError(404, "invite_gone", "This invite is no longer valid");

export function inviteUrl(env: Env, code: string): string {
  return `${env.PUBLIC_URL}/i/${code}`;
}

export async function peopleCount(env: Env): Promise<number> {
  const row = await env.DB.prepare(
    `select count(*) as n from users u
     where u.role is not null
        or exists (select 1 from pack_viewers v where v.user_id = u.id)`,
  ).first<{ n: number }>();
  return row?.n ?? 0;
}

async function findInvite(env: Env, code: string): Promise<InviteRow> {
  const invite = await env.DB.prepare(
    "select code, kind, pack_id, expires_at from invites where code = ? and used_at is null and expires_at > ?",
  )
    .bind(code, nowSeconds())
    .first<InviteRow>();
  if (!invite) throw gone();
  return invite;
}

async function findPack(env: Env, id: string | null): Promise<PackRow> {
  const pack = await env.DB.prepare(
    `select p.id, p.name, p.version, p.uploader_id, u.name as uploader_name
     from packs p join users u on u.id = p.uploader_id where p.id = ?`,
  )
    .bind(id)
    .first<PackRow>();
  if (!pack) throw gone();
  return pack;
}

async function requireRoom(env: Env, user: User): Promise<void> {
  if (user.role) return;
  const viewing = await env.DB.prepare("select 1 as x from pack_viewers where user_id = ? limit 1")
    .bind(user.id)
    .first();
  if (viewing) return;
  if ((await peopleCount(env)) >= numberVar(env, "MAX_USERS")) {
    throw new HttpError(403, "server_full", "This server has no room for more people");
  }
}

export async function acceptInvite(
  env: Env,
  user: User,
  code: string,
): Promise<{ kind: "uploader" | "viewer"; pack_id: string | null }> {
  const invite = await findInvite(env, code);
  const pack = invite.kind === "viewer" ? await findPack(env, invite.pack_id) : null;
  await requireRoom(env, user);

  if (!pack) {
    if (user.role) return { kind: "uploader", pack_id: null };
    const claim = await env.DB.prepare(
      "update invites set used_at = ? where code = ? and used_at is null and expires_at > ?",
    )
      .bind(nowSeconds(), code, nowSeconds())
      .run();
    if (claim.meta.changes !== 1) throw gone();
    await env.DB.prepare("update users set role = 'uploader' where id = ? and role is null")
      .bind(user.id)
      .run();
    return { kind: "uploader", pack_id: null };
  }

  if (user.role !== "owner" && user.id !== pack.uploader_id) {
    await env.DB.prepare("insert or ignore into pack_viewers (pack_id, user_id) values (?, ?)")
      .bind(pack.id, user.id)
      .run();
  }
  return { kind: "viewer", pack_id: pack.id };
}

export async function createUploaderInvite(req: Request, env: Env): Promise<Response> {
  const user = await requireUser(req, env);
  if (user.role !== "owner") throw new HttpError(403, "forbidden", "Only the owner can do this");
  const code = randomToken();
  const expiresAt = nowSeconds() + DAY;
  await env.DB.prepare(
    "insert into invites (code, kind, created_by, expires_at) values (?, 'uploader', ?, ?)",
  )
    .bind(code, user.id, expiresAt)
    .run();
  return json({ url: inviteUrl(env, code), expires_at: expiresAt });
}

export async function showInvite(req: Request, env: Env, code: string): Promise<Response> {
  await requireUser(req, env);
  const invite = await findInvite(env, code);
  let pack = null;
  if (invite.kind === "viewer") {
    const row = await findPack(env, invite.pack_id);
    pack = {
      id: row.id,
      name: row.name,
      version: row.version,
      uploader: { id: row.uploader_id, name: row.uploader_name },
    };
  }
  return json({ kind: invite.kind, expires_at: invite.expires_at, pack });
}

export async function acceptInviteRoute(req: Request, env: Env, code: string): Promise<Response> {
  const user = await requireUser(req, env);
  return json(await acceptInvite(env, user, code));
}

export function invitePage(env: Env, code: string): Response {
  const headers = { "content-type": "text/html; charset=utf-8" };
  if (!CODE_PATTERN.test(code)) {
    return new Response("<!doctype html><meta charset=\"utf-8\"><title>Invite</title><p>This invite link is not valid.</p>", {
      status: 404,
      headers,
    });
  }
  const link = inviteUrl(env, code);
  const html = `<!doctype html><meta charset="utf-8"><title>Invite</title><p>Paste this link into RON Mod Manager to join.</p><p><code>${link}</code></p>`;
  return new Response(html, { headers });
}
