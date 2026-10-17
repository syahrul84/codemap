-- The top-level keys a call sends (body and query), checked against the
-- backend action's Form Request rules and request reads.
ALTER TABLE api_calls ADD COLUMN sends_json TEXT;
