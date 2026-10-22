-- One model of UI views for every framework: Vue / React components, SwiftUI
-- views and Compose functions, and which view shows which. Each framework
-- rebuilds its own rows; the tools read them without caring which it is.

CREATE TABLE views (
    file_id   INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    framework TEXT    NOT NULL,   -- vue | swiftui | compose
    name      TEXT    NOT NULL,
    path      TEXT    NOT NULL,
    line      INTEGER NOT NULL,
    kind      TEXT                -- component | page | layout | view | composable
);

CREATE INDEX views_name ON views(name COLLATE NOCASE);
CREATE INDEX views_path ON views(path);

CREATE TABLE view_uses (
    framework TEXT    NOT NULL,
    from_path TEXT    NOT NULL,
    from_name TEXT,
    to_name   TEXT    NOT NULL,
    to_path   TEXT,
    line      INTEGER NOT NULL
);

CREATE INDEX view_uses_to   ON view_uses(to_path);
CREATE INDEX view_uses_from ON view_uses(from_path);
