-- Phase 3: HTTP routes. Routes are rebuilt as a whole set whenever a
-- route-relevant file changes, so they reference files by path (artisan-only
-- routes from packages have no file at all).

CREATE TABLE routes (
    id                INTEGER PRIMARY KEY,
    seq               INTEGER NOT NULL,          -- registration order
    methods           TEXT    NOT NULL,          -- GET|HEAD, PUT|PATCH, ...
    uri               TEXT    NOT NULL,          -- Laravel format: no leading slash, root is "/"
    name              TEXT,
    action_kind       TEXT    NOT NULL,          -- controller|closure|view|redirect|health|fallback|unresolved
    action_raw        TEXT,
    controller_fqn    TEXT,
    controller_method TEXT,
    middleware_json   TEXT    NOT NULL DEFAULT '[]',
    prefix            TEXT,
    domain            TEXT,
    file              TEXT,
    line              INTEGER,
    source            TEXT    NOT NULL,          -- static|artisan
    unresolved_reason TEXT,
    extra_json        TEXT
);

CREATE INDEX routes_name       ON routes(name);
CREATE INDEX routes_uri        ON routes(uri);
CREATE INDEX routes_controller ON routes(controller_fqn COLLATE NOCASE);
