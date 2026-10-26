CREATE TABLE pack_versions (
  id integer primary key autoincrement,
  pack_id text not null,
  version text not null,
  published_at integer not null,
  notes text not null
);
CREATE INDEX pack_versions_pack ON pack_versions (pack_id, id);
