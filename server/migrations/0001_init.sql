CREATE TABLE users (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  role TEXT CHECK (role IN ('owner', 'uploader')),
  created_at INTEGER NOT NULL
);

CREATE TABLE sessions (
  token_hash TEXT PRIMARY KEY,
  user_id TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  last_used INTEGER NOT NULL
);
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE logins (
  id TEXT PRIMARY KEY,
  app_redirect TEXT NOT NULL,
  app_state TEXT NOT NULL,
  code_challenge TEXT NOT NULL,
  code_hash TEXT,
  user_id TEXT,
  expires_at INTEGER NOT NULL
);

CREATE TABLE invites (
  code TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('uploader', 'viewer')),
  pack_id TEXT,
  created_by TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);

CREATE TABLE packs (
  id TEXT PRIMARY KEY,
  uploader_id TEXT NOT NULL,
  name TEXT NOT NULL,
  version TEXT NOT NULL,
  manifest TEXT,
  published_at INTEGER
);

CREATE TABLE pack_viewers (
  pack_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  PRIMARY KEY (pack_id, user_id)
);

CREATE TABLE blobs (
  sha256 TEXT PRIMARY KEY,
  size INTEGER NOT NULL
);

CREATE TABLE pack_blobs (
  pack_id TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  PRIMARY KEY (pack_id, sha256)
);
CREATE INDEX pack_blobs_sha ON pack_blobs (sha256);

CREATE TABLE publishes (
  id TEXT PRIMARY KEY,
  pack_id TEXT NOT NULL,
  user_id TEXT NOT NULL,
  name TEXT NOT NULL,
  version TEXT NOT NULL,
  expires_at INTEGER NOT NULL
);

CREATE TABLE publish_blobs (
  publish_id TEXT NOT NULL,
  sha256 TEXT NOT NULL,
  size INTEGER NOT NULL,
  verified INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (publish_id, sha256)
);

CREATE TABLE op_counts (
  month TEXT PRIMARY KEY,
  class_a INTEGER NOT NULL DEFAULT 0
);
