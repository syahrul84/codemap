-- Phase 11: translations. Keys defined per locale and keys used in code, for
-- the frontend (vue-i18n / nuxt-i18n) and Laravel (lang/, __()).

CREATE TABLE i18n_keys (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    source  TEXT    NOT NULL,   -- vue | laravel
    locale  TEXT    NOT NULL,
    key     TEXT    NOT NULL
);

CREATE INDEX i18n_keys_key ON i18n_keys(source, key);
CREATE INDEX i18n_keys_file ON i18n_keys(file_id);

CREATE TABLE i18n_uses (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    source  TEXT    NOT NULL,   -- vue | laravel
    key     TEXT    NOT NULL,   -- or a static prefix when dynamic
    line    INTEGER NOT NULL,
    dynamic INTEGER NOT NULL DEFAULT 0
);

CREATE INDEX i18n_uses_key ON i18n_uses(source, key);
CREATE INDEX i18n_uses_file ON i18n_uses(file_id);
