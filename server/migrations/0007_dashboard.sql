ALTER TABLE packs ADD COLUMN last_downloaded_at integer;
ALTER TABLE packs ADD COLUMN download_count integer NOT NULL DEFAULT 0;
ALTER TABLE packs ADD COLUMN last_used_at integer;

CREATE TABLE tracking (
  id integer PRIMARY KEY CHECK (id = 1),
  started_at integer NOT NULL
);
INSERT INTO tracking (id, started_at) VALUES (1, unixepoch());

CREATE TABLE storage_samples (
  taken_at integer PRIMARY KEY,
  bytes integer NOT NULL,
  extra_bytes integer NOT NULL
);

CREATE TABLE admin_sessions (
  token_hash text PRIMARY KEY,
  user_id text NOT NULL,
  expires_at integer NOT NULL
);
