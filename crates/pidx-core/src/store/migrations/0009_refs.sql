-- Phase 12: references for "find usages". PHP rows are written per file by
-- the PHP extractor; JS / Vue rows by the Vue registry pass.

CREATE TABLE refs (
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    kind    TEXT    NOT NULL,   -- class|call|call_unknown|function|route|http|import|use
    target  TEXT    NOT NULL,
    line    INTEGER NOT NULL
);

CREATE INDEX refs_target ON refs(target COLLATE NOCASE);
CREATE INDEX refs_file   ON refs(file_id);
