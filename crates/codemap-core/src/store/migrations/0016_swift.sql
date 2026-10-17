-- Swift files: per-file facts (types, SwiftUI views, screens, translations)
-- written when a file is (re)parsed; screens are rebuilt as a set from them.

CREATE TABLE swift_files (
    file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    facts_json TEXT    NOT NULL
);

CREATE INDEX swift_files_path ON swift_files(path);
