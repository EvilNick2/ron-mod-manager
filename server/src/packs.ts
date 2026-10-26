import { requireUser, type User } from "./auth";
import type { Env } from "./env";
import { HttpError, json } from "./http";

const PACK_ID = /^[A-Za-z0-9-]{1,64}$/;
const HASH = /^[0-9a-f]{64}$/;

// Binds: owner flag, user id, user id. Applies to a `packs p` row.
const VISIBLE = `p.manifest is not null and p.published_at is not null and (
  ? = 1 or p.uploader_id = ?
  or exists (select 1 from pack_viewers v where v.pack_id = p.id and v.user_id = ?))`;

const isOwner = (role: User["role"]) => (role === "owner" ? 1 : 0);

export async function visiblePacks(env: Env, user: User) {
  const rows = await env.DB.prepare(
    `select p.id, p.name, p.version, p.uploader_id, u.name as uploader_name,
            exists (select 1 from pack_viewers v where v.pack_id = p.id and v.user_id = ?) as viewing
     from packs p left join users u on u.id = p.uploader_id
     where ${VISIBLE} order by p.name, p.id`,
  )
    .bind(user.id, isOwner(user.role), user.id, user.id)
    .all<{
      id: string;
      name: string;
      version: string;
      uploader_id: string;
      uploader_name: string | null;
      viewing: number;
    }>();
  return rows.results.map((r) => ({
    id: r.id,
    name: r.name,
    version: r.version,
    uploader: { id: r.uploader_id, name: r.uploader_name ?? "" },
    access:
      r.uploader_id === user.id
        ? ("uploader" as const)
        : r.viewing
          ? ("viewer" as const)
          : ("owner" as const),
  }));
}

export async function me(req: Request, env: Env): Promise<Response> {
  const user = await requireUser(req, env);
  return json({ ...user, packs: await visiblePacks(env, user) });
}

export async function getManifest(req: Request, env: Env, packId: string): Promise<Response> {
  const user = await requireUser(req, env);
  const gone = new HttpError(404, "pack_gone", "This pack is not available");
  if (!PACK_ID.test(packId) || /^[0-9]+$/.test(packId)) throw gone;
  const row = await env.DB.prepare(`select p.manifest from packs p where p.id = ? and ${VISIBLE}`)
    .bind(packId, isOwner(user.role), user.id, user.id)
    .first<{ manifest: string }>();
  if (!row) throw gone;
  return new Response(row.manifest, { headers: { "content-type": "application/json" } });
}

function rangeSatisfiable(header: string | null, size: number): boolean {
  const m = header ? /^bytes=(\d*)-(\d*)$/.exec(header) : null;
  if (!m || (!m[1] && !m[2])) return true;
  if (!m[1]) return Number(m[2]) > 0;
  const start = Number(m[1]);
  return start < size && (!m[2] || Number(m[2]) >= start);
}

export async function getBlob(req: Request, env: Env, sha: string): Promise<Response> {
  const user = await requireUser(req, env);
  const gone = () => new HttpError(404, "blob_gone", "This file is not available");
  if (!HASH.test(sha)) throw gone();
  const row = await env.DB.prepare(
    `select bl.size from pack_blobs b join packs p on p.id = b.pack_id
     join blobs bl on bl.sha256 = b.sha256
     where b.sha256 = ? and ${VISIBLE} limit 1`,
  )
    .bind(sha, isOwner(user.role), user.id, user.id)
    .first<{ size: number }>();
  if (!row) throw gone();
  if (!rangeSatisfiable(req.headers.get("range"), row.size)) {
    throw new HttpError(416, "bad_range", "Range not satisfiable", { "content-range": `bytes */${row.size}` });
  }

  const obj = await env.PACKS.get(`blobs/${sha}`, { range: req.headers });
  if (!obj) throw gone();
  const headers: Record<string, string> = {
    "content-type": "application/octet-stream",
    "accept-ranges": "bytes",
  };
  if (!("body" in obj)) throw gone();
  const r = obj.range as { offset?: number; length?: number; suffix?: number } | undefined;
  const start = r?.suffix !== undefined ? obj.size - Math.min(r.suffix, obj.size) : (r?.offset ?? 0);
  const length = r?.suffix !== undefined ? Math.min(r.suffix, obj.size) : (r?.length ?? obj.size - start);
  if (req.headers.has("range") && r && (start > 0 || length < obj.size)) {
    headers["content-range"] = `bytes ${start}-${start + length - 1}/${obj.size}`;
    headers["content-length"] = String(length);
    return new Response(obj.body, { status: 206, headers });
  }
  headers["content-length"] = String(obj.size);
  return new Response(obj.body, { headers });
}
