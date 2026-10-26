import { IDLE_EXPIRY } from "./auth";
import type { Env } from "./env";
import { countClassA } from "./storage";

const MAX_DELETES = 40;
const PREFIX = "blobs/";
// Cleanup waits one cron interval past expiry, which also outlasts every presigned URL.
const EXPIRY_GRACE = 900;

// Cancelled publishes expire now, so cleanup waits the full grace and in-flight uploads finish first.
export const cancelledExpiry = (now: number) => now;

const ABANDONED_PACKS = `select id from packs where manifest is null
  and not exists (select 1 from publishes where pack_id = packs.id and expires_at > ?)`;

const OPEN_PUBLISH_HASHES = `select pb.sha256 from publish_blobs pb
  join publishes p on p.id = pb.publish_id where p.expires_at > ?`;

async function deleteObjects(env: Env, hashes: string[]): Promise<void> {
  if (hashes.length === 0) return;
  await env.PACKS.delete(hashes.map((h) => PREFIX + h));
  await countClassA(env, 1);
}

// Liveness is checked in D1 just before each R2 delete. A new publish reserving the same hash in that gap is not guarded, since uploading takes far longer.
export async function cleanup(env: Env, now: number): Promise<void> {
  let budget = MAX_DELETES;

  await env.DB.batch([
    env.DB.prepare("delete from logins where expires_at <= ?").bind(now),
    env.DB.prepare("delete from invites where expires_at <= ?").bind(now),
    env.DB.prepare("delete from sessions where last_used < ?").bind(now - IDLE_EXPIRY),
    env.DB.prepare(`delete from pack_viewers where pack_id in (${ABANDONED_PACKS})`).bind(now),
    env.DB.prepare(`delete from invites where pack_id in (${ABANDONED_PACKS})`).bind(now),
    env.DB.prepare(`delete from packs where id in (${ABANDONED_PACKS})`).bind(now),
  ]);

  const cutoff = now - EXPIRY_GRACE;
  const expired = await env.DB.prepare(
    "select id from publishes where expires_at <= ? order by expires_at limit ?",
  )
    .bind(cutoff, MAX_DELETES)
    .all<{ id: string }>();
  for (const { id } of expired.results) {
    const rows = await env.DB.prepare(
      `select distinct sha256 from publish_blobs
       where publish_id = ?
         and sha256 not in (select sha256 from blobs)
         and sha256 not in (${OPEN_PUBLISH_HASHES})`,
    )
      .bind(id, cutoff)
      .all<{ sha256: string }>();
    const slice = rows.results.map((r) => r.sha256).slice(0, budget);
    await deleteObjects(env, slice);
    budget -= slice.length;
    if (slice.length) {
      await env.DB.prepare(
        "delete from publish_blobs where publish_id = ? and sha256 in (select value from json_each(?))",
      )
        .bind(id, JSON.stringify(slice))
        .run();
    }
    if (rows.results.length > slice.length) return;
    await env.DB.batch([
      env.DB.prepare("delete from publish_blobs where publish_id = ?").bind(id),
      env.DB.prepare("delete from publishes where id = ?").bind(id),
    ]);
    if (budget === 0) return;
  }

  const orphans = await env.DB.prepare(
    `delete from blobs where sha256 in (
       select sha256 from blobs b
       where not exists (select 1 from pack_blobs where sha256 = b.sha256)
         and not exists (select 1 from publish_blobs where sha256 = b.sha256)
       limit ?)
     and not exists (select 1 from pack_blobs where sha256 = blobs.sha256)
     and not exists (select 1 from publish_blobs where sha256 = blobs.sha256)
     returning sha256`,
  )
    .bind(budget)
    .all<{ sha256: string }>();
  await deleteObjects(env, orphans.results.map((r) => r.sha256));
  budget -= orphans.results.length;
  if (budget <= 0) return;

  const state = await env.DB.prepare("select cursor from cleanup_state where id = 1").first<{
    cursor: string | null;
  }>();
  const page = await env.PACKS.list({
    prefix: PREFIX,
    startAfter: state?.cursor ?? undefined,
  });
  await countClassA(env, 1);
  const keys = page.objects.map((o) => o.key);
  let next: string | null = page.truncated && keys.length ? keys[keys.length - 1] : null;
  if (keys.length) {
    const live = await env.DB.prepare(
      `select value from json_each(?)
       where substr(value, ?) in (select sha256 from blobs)
          or substr(value, ?) in (${OPEN_PUBLISH_HASHES})`,
    )
      .bind(JSON.stringify(keys), PREFIX.length + 1, PREFIX.length + 1, cutoff)
      .all<{ value: string }>();
    const keep = new Set(live.results.map((r) => r.value));
    const dead = keys.filter((k) => !keep.has(k));
    const batch = dead.slice(0, budget);
    if (batch.length) {
      await deleteObjects(env, batch.map((k) => k.slice(PREFIX.length)));
      if (dead.length > budget) next = batch[batch.length - 1];
    }
  }
  await env.DB.prepare(
    `insert into cleanup_state (id, cursor) values (1, ?)
     on conflict (id) do update set cursor = excluded.cursor`,
  )
    .bind(next)
    .run();
}
