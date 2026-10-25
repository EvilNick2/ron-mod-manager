export interface Env {
  DB: D1Database;
  PACKS: R2Bucket;
  EXTRA_BUCKET?: R2Bucket;
  USER_LIMIT: RateLimit;

  OWNER_DISCORD_ID: string;
  DISCORD_CLIENT_ID: string;
  PUBLIC_URL: string;
  STORAGE_CAP_BYTES: string;
  MAX_USERS: string;
  OP_BUDGET: string;
  R2_ACCOUNT_ID: string;
  R2_BUCKET_NAME: string;

  DISCORD_CLIENT_SECRET: string;
  R2_ACCESS_KEY_ID: string;
  R2_SECRET_ACCESS_KEY: string;
}
