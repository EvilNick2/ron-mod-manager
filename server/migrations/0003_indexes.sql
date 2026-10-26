CREATE INDEX publish_blobs_sha ON publish_blobs (sha256);
CREATE INDEX publishes_expires ON publishes (expires_at);
CREATE INDEX pack_viewers_user ON pack_viewers (user_id);
