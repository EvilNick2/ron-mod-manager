import type { Env } from "./env";

const month = () => new Date().toISOString().slice(0, 7);

export async function classAUsed(env: Env): Promise<number> {
  const row = await env.DB.prepare("select class_a from op_counts where month = ?")
    .bind(month())
    .first<{ class_a: number }>();
  return row?.class_a ?? 0;
}

export async function countClassA(env: Env, n: number): Promise<void> {
  if (n <= 0) return;
  await env.DB.prepare(
    `insert into op_counts (month, class_a) values (?, ?)
     on conflict (month) do update set class_a = class_a + excluded.class_a`,
  )
    .bind(month(), n)
    .run();
}

export async function extraBucketBytes(env: Env): Promise<number> {
  if (!env.EXTRA_BUCKET) return 0;
  let total = 0;
  let lists = 0;
  let cursor: string | undefined;
  do {
    const page = await env.EXTRA_BUCKET.list({ cursor });
    lists++;
    for (const o of page.objects) total += o.size;
    cursor = page.truncated ? page.cursor : undefined;
  } while (cursor);
  await countClassA(env, lists);
  return total;
}

export const DB_BYTES_SQL = `(
  (select coalesce(sum(size), 0) from blobs) +
  (select coalesce(sum(size), 0) from (
    select sha256, max(size) as size from publish_blobs
    where sha256 not in (select sha256 from blobs)
    group by sha256
  ))
)`;

export async function storageUsed(env: Env): Promise<number> {
  const row = await env.DB.prepare(`select ${DB_BYTES_SQL} as n`).first<{ n: number }>();
  return (row?.n ?? 0) + (await extraBucketBytes(env));
}
