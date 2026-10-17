-- Exact lookups by kind and target (`kind = 'route' AND target = ?`) and
-- lists of one kind use this; refs_target is case-insensitive.
CREATE INDEX IF NOT EXISTS refs_kind_target ON refs(kind, target);
