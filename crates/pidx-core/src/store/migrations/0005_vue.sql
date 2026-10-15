-- Milestone 2 / Phase 8: JS, TS and Vue modules. Per-file facts are written
-- when a file is (re)parsed; names, kinds and component uses depend on the
-- whole project (detection, registrations) and are rebuilt as a set.

CREATE TABLE js_modules (
    file_id    INTEGER PRIMARY KEY REFERENCES files(id) ON DELETE CASCADE,
    path       TEXT    NOT NULL,
    -- component|page|layout|module (set by the registry pass)
    kind       TEXT    NOT NULL DEFAULT 'module',
    -- component name after naming rules (registry pass)
    name       TEXT,
    facts_json TEXT    NOT NULL
);

CREATE INDEX js_modules_path ON js_modules(path);
CREATE INDEX js_modules_name ON js_modules(name COLLATE NOCASE);

-- Which component renders which: one row per tag occurrence (first per file).
CREATE TABLE component_uses (
    from_path TEXT    NOT NULL,
    tag       TEXT    NOT NULL,
    line      INTEGER NOT NULL,
    -- project file, or NULL when external/unresolved
    to_path   TEXT,
    -- local|global|auto|external|unresolved
    via       TEXT    NOT NULL,
    package   TEXT
);

CREATE INDEX component_uses_to   ON component_uses(to_path);
CREATE INDEX component_uses_from ON component_uses(from_path);
