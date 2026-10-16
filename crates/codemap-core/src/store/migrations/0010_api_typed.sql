-- Fields the TypeScript type of a call's response requires (`useApi()<T>()`),
-- compared with what the endpoint returns.
ALTER TABLE api_calls ADD COLUMN typed_json TEXT;
