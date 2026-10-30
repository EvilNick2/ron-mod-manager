CREATE TABLE pack_editors (
  pack_id text not null,
  user_id text not null,
  primary key (pack_id, user_id)
);
CREATE INDEX pack_editors_user ON pack_editors (user_id);

CREATE TABLE invites_new (
  code TEXT PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('uploader', 'viewer', 'editor')),
  pack_id TEXT,
  created_by TEXT NOT NULL,
  expires_at INTEGER NOT NULL,
  used_at INTEGER
);
INSERT INTO invites_new (code, kind, pack_id, created_by, expires_at, used_at)
  SELECT code, kind, pack_id, created_by, expires_at, used_at FROM invites;
DROP TABLE invites;
ALTER TABLE invites_new RENAME TO invites;

ALTER TABLE pack_versions ADD COLUMN publisher text;
ALTER TABLE publishes ADD COLUMN base_version text;
