PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rectification_cases (
    id TEXT PRIMARY KEY NOT NULL,
    display_name TEXT NOT NULL,
    tracking_number TEXT NOT NULL,
    customs_form_number TEXT,
    status TEXT NOT NULL,
    has_unseen_updates INTEGER NOT NULL DEFAULT 0 CHECK (has_unseen_updates IN (0, 1)),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_cases_tracking_number
    ON rectification_cases(tracking_number);

CREATE INDEX IF NOT EXISTS idx_cases_updated_at
    ON rectification_cases(updated_at DESC);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (1, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
