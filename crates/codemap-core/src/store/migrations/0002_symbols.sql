-- Phase 2: PHP symbols, their extends/implements/trait references, FTS, and
-- per-file extractor versions (what still needs parsing).

CREATE TABLE symbols (
    id          INTEGER PRIMARY KEY,
    file_id     INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind        TEXT    NOT NULL,   -- class|interface|trait|enum|method|function|property|const|enum_case
    name        TEXT    NOT NULL,
    fqn         TEXT    NOT NULL,   -- App\Models\User, App\Models\User::save, App\Models\User::$fillable
    parent_fqn  TEXT,
    signature   TEXT    NOT NULL,
    visibility  TEXT,
    is_static   INTEGER NOT NULL DEFAULT 0,
    is_abstract INTEGER NOT NULL DEFAULT 0,
    is_final    INTEGER NOT NULL DEFAULT 0,
    start_line  INTEGER NOT NULL,
    end_line    INTEGER NOT NULL,
    doc_summary TEXT,
    meta_json   TEXT
);

CREATE INDEX symbols_file   ON symbols(file_id);
CREATE INDEX symbols_name   ON symbols(name COLLATE NOCASE);
CREATE INDEX symbols_fqn    ON symbols(fqn COLLATE NOCASE);
CREATE INDEX symbols_parent ON symbols(parent_fqn COLLATE NOCASE);

CREATE TABLE symbol_refs (
    symbol_id  INTEGER NOT NULL REFERENCES symbols(id) ON DELETE CASCADE,
    relation   TEXT    NOT NULL,    -- extends|implements|uses_trait
    target_fqn TEXT    NOT NULL
);

CREATE INDEX symbol_refs_symbol ON symbol_refs(symbol_id);
CREATE INDEX symbol_refs_target ON symbol_refs(target_fqn COLLATE NOCASE);

-- Trigram tokenizer: case-insensitive substring search ("Service" finds "OrderService").
CREATE VIRTUAL TABLE symbols_fts USING fts5(
    name, fqn, doc_summary,
    content = 'symbols', content_rowid = 'id', tokenize = 'trigram'
);

CREATE TRIGGER symbols_fts_insert AFTER INSERT ON symbols BEGIN
    INSERT INTO symbols_fts (rowid, name, fqn, doc_summary)
    VALUES (new.id, new.name, new.fqn, new.doc_summary);
END;

CREATE TRIGGER symbols_fts_delete AFTER DELETE ON symbols BEGIN
    INSERT INTO symbols_fts (symbols_fts, rowid, name, fqn, doc_summary)
    VALUES ('delete', old.id, old.name, old.fqn, old.doc_summary);
END;

CREATE TRIGGER symbols_fts_update AFTER UPDATE ON symbols BEGIN
    INSERT INTO symbols_fts (symbols_fts, rowid, name, fqn, doc_summary)
    VALUES ('delete', old.id, old.name, old.fqn, old.doc_summary);
    INSERT INTO symbols_fts (rowid, name, fqn, doc_summary)
    VALUES (new.id, new.name, new.fqn, new.doc_summary);
END;

-- A file is (re)parsed by an extractor when it has no row with that extractor's
-- current version. Modified files get a new file id, so they are pending too.
CREATE TABLE extractor_state (
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    extractor TEXT    NOT NULL,
    version   INTEGER NOT NULL,
    PRIMARY KEY (file_id, extractor)
) WITHOUT ROWID;
