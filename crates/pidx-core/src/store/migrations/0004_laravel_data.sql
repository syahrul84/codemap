-- Phase 4: Eloquent models, the database schema replayed from migrations
-- (and schema dumps), and Form Request rules. Each set is rebuilt as a whole,
-- so rows reference files by path and carry file:line provenance.

CREATE TABLE models (
    fqn            TEXT PRIMARY KEY,
    kind           TEXT    NOT NULL,      -- model|pivot|user
    parent_fqn     TEXT,
    table_name     TEXT    NOT NULL,
    table_inferred INTEGER NOT NULL,
    connection     TEXT,
    primary_key    TEXT,
    fillable_json  TEXT,
    guarded_json   TEXT,
    hidden_json    TEXT,
    casts_json     TEXT,
    appends_json   TEXT,
    traits_json    TEXT    NOT NULL DEFAULT '[]',
    file           TEXT    NOT NULL,
    line           INTEGER NOT NULL,
    notes_json     TEXT
) WITHOUT ROWID;

CREATE INDEX models_table ON models(table_name);

CREATE TABLE model_relations (
    model_fqn         TEXT    NOT NULL,
    method            TEXT    NOT NULL,
    type              TEXT    NOT NULL,   -- hasMany, belongsTo, ...
    related_fqn       TEXT,
    foreign_key       TEXT,
    owner_key         TEXT,
    pivot_table       TEXT,
    through_fqn       TEXT,
    morph_name        TEXT,
    line              INTEGER NOT NULL,
    unresolved_reason TEXT
);

CREATE INDEX model_relations_model ON model_relations(model_fqn);

CREATE TABLE model_accessors (
    model_fqn TEXT    NOT NULL,
    attribute TEXT    NOT NULL,
    method    TEXT    NOT NULL,
    style     TEXT    NOT NULL,           -- legacy_get|legacy_set|attribute_make
    line      INTEGER NOT NULL
);

CREATE TABLE model_scopes (
    model_fqn  TEXT    NOT NULL,
    scope_name TEXT    NOT NULL,
    method     TEXT    NOT NULL,
    line       INTEGER NOT NULL
);

CREATE TABLE db_tables (
    name            TEXT PRIMARY KEY,
    created_in      TEXT,                 -- file:line
    last_changed_in TEXT,
    dropped         INTEGER NOT NULL DEFAULT 0,
    renamed_from    TEXT,
    confidence      TEXT    NOT NULL DEFAULT 'high'
) WITHOUT ROWID;

CREATE TABLE db_columns (
    table_name      TEXT    NOT NULL,
    name            TEXT    NOT NULL,
    position        INTEGER NOT NULL,
    type            TEXT    NOT NULL,
    nullable        INTEGER NOT NULL,
    default_raw     TEXT,
    unsigned        INTEGER NOT NULL,
    is_unique       INTEGER NOT NULL,
    indexed         INTEGER NOT NULL,
    is_primary      INTEGER NOT NULL,
    auto_increment  INTEGER NOT NULL,
    fk_table        TEXT,
    fk_column       TEXT,
    created_in      TEXT,
    last_changed_in TEXT,
    confidence      TEXT    NOT NULL,     -- high|low (inside a condition, inferred FK, ...)
    PRIMARY KEY (table_name, name)
) WITHOUT ROWID;

CREATE TABLE form_requests (
    fqn               TEXT PRIMARY KEY,
    rules_json        TEXT,
    authorize_raw     TEXT,
    file              TEXT    NOT NULL,
    line              INTEGER NOT NULL,
    unresolved_reason TEXT
) WITHOUT ROWID;
