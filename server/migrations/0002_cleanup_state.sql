CREATE TABLE cleanup_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  cursor TEXT
);
