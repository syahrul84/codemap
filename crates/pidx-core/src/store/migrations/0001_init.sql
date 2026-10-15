-- Phase 1: index metadata, scanned files, warnings.
-- Later migrations add extractor tables with `file_id ... ON DELETE CASCADE`,
-- so deleting a file row removes every fact extracted from it.

CREATE TABLE meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;

CREATE TABLE files (
    id         INTEGER PRIMARY KEY,
    path       TEXT    NOT NULL UNIQUE,  -- relative to the project root, '/' separators
    hash       TEXT    NOT NULL,         -- blake3 hex
    size       INTEGER NOT NULL,
    mtime_ns   INTEGER NOT NULL,
    lang       TEXT    NOT NULL,         -- php | sql | json
    indexed_at TEXT    NOT NULL
);

CREATE TABLE warnings (
    id      INTEGER PRIMARY KEY,
    file_id INTEGER REFERENCES files(id) ON DELETE CASCADE,
    path    TEXT,                        -- set even when the file is not indexed (e.g. skipped)
    line    INTEGER,
    adapter TEXT    NOT NULL,
    message TEXT    NOT NULL
);

CREATE INDEX warnings_file_id ON warnings(file_id);
CREATE INDEX warnings_adapter ON warnings(adapter);
