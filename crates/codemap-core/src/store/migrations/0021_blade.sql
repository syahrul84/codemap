-- Blade templates: per-file facts (layout, includes, components, forms,
-- variables) written when a template is (re)read; the `views` and
-- `view_uses` rows of framework `blade` are rebuilt as a set from them.

CREATE TABLE blade_files (
    file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    facts_json TEXT    NOT NULL
);

CREATE INDEX blade_files_path ON blade_files(path);
