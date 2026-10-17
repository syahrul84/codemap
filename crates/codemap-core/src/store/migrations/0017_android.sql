-- Kotlin, Java and Android resource files: per-file facts (types, Compose
-- functions, screens, manifest components, translations) written when a
-- file is (re)parsed; screens and translations are rebuilt as a set.

CREATE TABLE android_files (
    file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    facts_json TEXT    NOT NULL
);

CREATE INDEX android_files_path ON android_files(path);
