CREATE TABLE IF NOT EXISTS case_capture_progress (
    case_id TEXT PRIMARY KEY NOT NULL,
    current_step INTEGER NOT NULL DEFAULT 2 CHECK (current_step BETWEEN 2 AND 5),
    updated_at TEXT NOT NULL,
    completed_at TEXT,
    FOREIGN KEY (case_id) REFERENCES rectification_cases(id) ON DELETE CASCADE
);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (10, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
