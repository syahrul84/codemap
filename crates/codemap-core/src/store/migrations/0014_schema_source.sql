-- Where a table or column definition comes from (`migration`, `schema dump`,
-- `live:local`, `dump:structure`) and how it differs from the other source.
ALTER TABLE db_tables ADD COLUMN source TEXT;
ALTER TABLE db_tables ADD COLUMN drift TEXT;
ALTER TABLE db_columns ADD COLUMN source TEXT;
ALTER TABLE db_columns ADD COLUMN drift TEXT;
