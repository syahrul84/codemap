-- Phase 9: screens. Nuxt file-based pages and vue-router route records,
-- rebuilt as a set by the Vue registry pass.

CREATE TABLE pages (
    route_path   TEXT    NOT NULL,   -- `/orders/:id`, or `(dynamic)` when not readable
    name         TEXT,
    file         TEXT,               -- component file rendered for this route
    source       TEXT    NOT NULL,   -- nuxt | router
    defined_in   TEXT    NOT NULL,   -- file:line
    parent       TEXT,               -- parent route path (nested routes)
    layout       TEXT,
    middleware_json TEXT,
    redirect     TEXT,
    meta         TEXT,
    note         TEXT                -- why something is unresolved
);

CREATE INDEX pages_file ON pages(file);
CREATE INDEX pages_path ON pages(route_path);
