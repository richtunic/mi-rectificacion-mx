CREATE TABLE IF NOT EXISTS archived_cases (
    case_id TEXT PRIMARY KEY NOT NULL,
    archived_at TEXT NOT NULL,
    FOREIGN KEY (case_id) REFERENCES rectification_cases(id) ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_archived_cases_archived_at
    ON archived_cases(archived_at DESC);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (9, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
