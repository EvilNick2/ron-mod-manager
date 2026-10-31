import { nowSeconds, requireUser, type User } from "./auth";
import type { Env } from "./env";
import { HttpError, json, readJson } from "./http";
import { PACK_ID, VISIBLE, isOwner } from "./packs";

const FUTURE_SLACK = 300;

// Resolves read access and whether the caller is the creator, an editor or the server owner.
async function accessFor(env: Env, user: User, packId: string): Promise<{ counts: boolean }> {
  const gone = new HttpError(404, "pack_gone", "This pack is not available");
  if (!PACK_ID.test(packId) || /^[0-9]+$/.test(packId)) throw gone;
  const row = await env.DB.prepare(
    `select p.uploader_id = ? or exists (select 1 from pack_editors e where e.pack_id = p.id and e.user_id = ?) as privileged
     from packs p where p.id = ? and ${VISIBLE}`,
  )
    .bind(user.id, user.id, packId, isOwner(user.role), user.id, user.id, user.id)
    .first<{ privileged: number }>();
  if (!row) throw gone;
  return { counts: !row.privileged && user.role !== "owner" };
}

export async function recordDownload(req: Request, env: Env, packId: string): Promise<Response> {
  const user = await requireUser(req, env);
  const { counts } = await accessFor(env, user, packId);
  if (!counts) return json({ recorded: false });
  await env.DB.prepare(
    "update packs set last_downloaded_at = ?, download_count = download_count + 1 where id = ?",
  )
    .bind(nowSeconds(), packId)
    .run();
  return json({ recorded: true });
}

export async function recordUse(req: Request, env: Env, packId: string): Promise<Response> {
  const user = await requireUser(req, env);
  const { counts } = await accessFor(env, user, packId);
  const body = await readJson<{ played_at?: unknown }>(req);
  const at = body.played_at;
  if (typeof at !== "number" || !Number.isInteger(at) || at <= 0) {
    throw new HttpError(400, "bad_request", "played_at must be a positive integer");
  }
  if (!counts || at > nowSeconds() + FUTURE_SLACK) return json({ recorded: false });
  await env.DB.prepare(
    "update packs set last_used_at = ? where id = ? and (last_used_at is null or last_used_at < ?)",
  )
    .bind(at, packId, at)
    .run();
  return json({ recorded: true });
}
