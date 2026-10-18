-- Indexes over several columns (`$table->unique(['shelf_id', 'slug'])`),
-- as JSON: [{name, kind, columns, defined_in}].
ALTER TABLE db_tables ADD COLUMN indexes_json TEXT;
