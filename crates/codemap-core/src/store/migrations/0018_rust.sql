-- Rust files: per-file facts (module path, impls, commands, test ranges)
-- written when a file is (re)parsed; entry points are rebuilt as a set.

CREATE TABLE rust_files (
    file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    facts_json TEXT    NOT NULL
);

CREATE INDEX rust_files_path ON rust_files(path);
