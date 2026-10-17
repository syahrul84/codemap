-- The values a variable URL prefix can take (`${prefix(kind)}/contacts` with
-- `prefix = (k) => k === 'x' ? 'area/x' : 'area'`), tried in full when matching.
ALTER TABLE api_calls ADD COLUMN prefixes_json TEXT;
