-- Phase 10: HTTP calls made by the frontend, with the fields read from their
-- results. Matched to backend routes at query time (the backend may be a
-- linked project with its own index).

CREATE TABLE api_calls (
    file        TEXT    NOT NULL,
    line        INTEGER NOT NULL,
    method      TEXT,               -- NULL when the wrapper hides it
    url         TEXT    NOT NULL,   -- normalised: /orders/{}/items
    raw         TEXT    NOT NULL,
    via         TEXT    NOT NULL,
    base_var    INTEGER NOT NULL DEFAULT 0,
    external    INTEGER NOT NULL DEFAULT 0,
    fields_json TEXT                -- top-level fields read (heuristic)
);

CREATE INDEX api_calls_file ON api_calls(file);
CREATE INDEX api_calls_url  ON api_calls(url);
