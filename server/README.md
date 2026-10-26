# RON Pack Server

A Cloudflare Worker that lets people share modpacks through the RON Mod Manager desktop app. It uses D1 for accounts and pack records, R2 for the mod files, and Discord for sign-in. Uploads go straight from the app to R2 through short-lived presigned PUT URLs. Downloads stream through the Worker, which reads R2 through its binding.

The app has the maintainer's server built in. Anyone can run their own copy and point the app at it. This guide is for that.

## Costs

Read this before you deploy.

The cost limits below assume the Workers Free plan. On Workers Paid, Worker requests are billed and downloads have no limit at all.

- Workers Free and D1 Free stop working when you pass their limits. They do not bill you. Workers Free allows 100,000 requests a day across the account.
- R2 bills storage past 10 GB-month at $0.015 per GB-month. It bills Class A operations (writes and lists) past 1 million a month. R2 has no spending cap.
- R2 bills Class B operations (reads) past 10 million a month. Every download is one Class B read through the Worker. `OP_BUDGET` does not count them. Only the Workers Free cap of 100,000 requests a day keeps them under the free 10 million, at most about 3.1 million a month.

The server bounds R2 use with two settings:

- `STORAGE_CAP_BYTES` is the most the server will let people store. Default `20000000000` (20 GB). Publishes that would go over it are refused.
- `OP_BUDGET` is the most Class A operations the server will use in a month. Default `900000`. The server counts its own Class A operations in D1 and refuses work past the budget.

These limits only cover traffic that goes through this server. Set an R2 budget alert in the Cloudflare dashboard as well. Do not rely on the server alone.

The R2 free allowances are per account and shared with everything else on it. Other R2 data on the same account uses up the free 10 GB unless it sits in the bucket bound as `EXTRA_BUCKET`. Other Class A use shares the free 1 million operations.

With the defaults, if nothing else is on the account, storage past 10 GB costs at most about $0.15 a month (10 GB x $0.015). If other data already fills the free 10 GB, all 20 GB of the cap is billed, about $0.30 a month. In the same worst case the full `OP_BUDGET` of 900,000 Class A operations is billed too, about $4.05 a month at $4.50 per million. Downloads add nothing on Workers Free.

## Setup

You need a Cloudflare account, Node.js, and a Discord account. Run all commands from the `server/` folder.

1. Install dependencies and sign in to Cloudflare.

   ```
   npm install
   npx wrangler login
   ```

2. Create the R2 bucket.

   ```
   npx wrangler r2 bucket create ron-packs
   ```

   Keep the bucket private. Do not turn on the r2.dev URL. Do not attach a public custom domain to the bucket. Uploads use presigned PUT URLs and downloads go through the Worker.

3. Create the D1 database.

   ```
   npx wrangler d1 create ron-packs
   ```

   The output includes a `database_id`.

4. Edit `wrangler.jsonc`.
   - In `d1_databases`, set `database_id` to the id from step 3. Keep `database_name` the same as the name you created.
   - In `r2_buckets`, set `bucket_name` to your bucket name.
   - In `ratelimits`, `namespace_id` is a string you pick. Any unique positive integer works, for example `"1001"`. It only has to be unique among your account's rate limit namespaces.

5. Create a Discord application at https://discord.com/developers/applications.
   - Under OAuth2, add the redirect `<PUBLIC_URL>/auth/callback`. For example `https://packs.example.com/auth/callback`. It must match `PUBLIC_URL` exactly.
   - The server asks for the `identify` scope only.
   - Copy the Client ID and Client Secret.
   - Find your own Discord user id (turn on Developer Mode, right-click your name, Copy User ID). This becomes `OWNER_DISCORD_ID`.

6. Create an R2 API token in the Cloudflare dashboard (R2, Manage API Tokens).
   - Permission: Object Read and Write.
   - Scope: only the bucket from step 2.
   - Copy the Access Key ID and Secret Access Key. Also note your Cloudflare account id.

7. Set the vars in `wrangler.jsonc` under `vars`.

   | Var | Meaning |
   | --- | --- |
   | `OWNER_DISCORD_ID` | Discord user id of the owner. The owner can manage all users. |
   | `DISCORD_CLIENT_ID` | Client ID of the Discord application. |
   | `PUBLIC_URL` | Public base URL of the server, with no trailing slash. Used for the Discord redirect and invite links. |
   | `STORAGE_CAP_BYTES` | Storage cap in bytes. Default `"20000000000"`. |
   | `MAX_USERS` | Most people allowed on the server. Default `"30"`. |
   | `OP_BUDGET` | Monthly Class A operation budget. Default `"900000"`. |
   | `R2_ACCOUNT_ID` | Your Cloudflare account id. Used to build presigned URLs. |
   | `R2_BUCKET_NAME` | Name of the bucket from step 2. |

   All numeric vars are strings. A missing or invalid value makes the server return a `config` error.

   `EXTRA_BUCKET` is an optional second R2 binding. If you add it to `r2_buckets`, its size counts toward the storage cap. The binding name must be exactly `EXTRA_BUCKET`. Every publish lists that bucket to measure it, and those lists are Class A operations counted in `OP_BUDGET`. Leave it out if you do not need it.

8. Set the three secrets. Each command asks for the value.

   ```
   npx wrangler secret put DISCORD_CLIENT_SECRET
   npx wrangler secret put R2_ACCESS_KEY_ID
   npx wrangler secret put R2_SECRET_ACCESS_KEY
   ```

   If the Worker does not exist yet, wrangler offers to create it. Say yes.

9. Apply the migrations to the remote database. The argument is the database name or binding.

   ```
   npx wrangler d1 migrations apply DB --remote
   ```

10. Deploy.

    ```
    npx wrangler deploy
    ```

11. Optional: use your own domain. The domain must be on your Cloudflare account. Add this to `wrangler.jsonc`, set `PUBLIC_URL` to match, then deploy again.

    ```
    "routes": [{ "pattern": "packs.example.com", "custom_domain": true }]
    ```

    Without this the Worker is served from a `workers.dev` URL. In that case set `PUBLIC_URL` to that URL and use it in the Discord redirect.

12. Check it works. `GET <PUBLIC_URL>/health` should return `{"ok":true}`.

13. Set a budget alert for R2 in the Cloudflare dashboard.

## Rate limiting

Each user is limited to 300 requests per 60 seconds through the `USER_LIMIT` binding.

The binding works on Workers Free. Wrangler 4.152.0 deployed it to a Free account without complaint (checked 2026-10-26).

## Presigned upload checks

Uploads go straight to R2 through presigned PUT URLs. The server also verifies blob sizes before a publish commits, and the app checks hashes on download.

R2 enforces both signed headers. Checked live on 2026-10-26:

- A body longer than the signed `Content-Length` is refused with 403 `SignatureDoesNotMatch`.
- A body of the right length with different bytes is refused with 400 `BadDigest`.

So an object stored under `blobs/<sha256>` always has the bytes that hash names.

## Cleanup

A cron trigger runs every 15 minutes (`*/15 * * * *`). It removes abandoned publishes and unreferenced blobs. Each run deletes at most 40 objects, so a large backlog clears over several runs.

Deleting a pack or removing an uploader ends access at once, but the files leave R2 at 40 per run. A pack of about 280 files takes about 2 hours to clear. Cancelled publishes, and publishes of a deleted pack, keep counting toward the storage cap until cleanup has removed their files, which takes at least 5 minutes.

## Development

```
npm install
npx vitest run
npx tsc --noEmit
```

`npx wrangler dev` runs the Worker locally.
