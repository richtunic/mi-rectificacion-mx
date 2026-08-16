CREATE TABLE IF NOT EXISTS tracking_snapshots (
    id TEXT PRIMARY KEY NOT NULL,
    case_id TEXT NOT NULL REFERENCES rectification_cases(id) ON DELETE CASCADE,
    provider TEXT NOT NULL,
    fetched_at TEXT NOT NULL,
    raw_response TEXT NOT NULL,
    error_message TEXT
);

CREATE INDEX IF NOT EXISTS idx_tracking_snapshots_case
    ON tracking_snapshots(case_id, fetched_at DESC);

CREATE TABLE IF NOT EXISTS tracking_events (
    id TEXT PRIMARY KEY NOT NULL,
    case_id TEXT NOT NULL REFERENCES rectification_cases(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    occurred_at TEXT,
    description TEXT NOT NULL,
    location TEXT,
    source TEXT NOT NULL,
    is_seen INTEGER NOT NULL DEFAULT 0 CHECK (is_seen IN (0, 1)),
    created_at TEXT NOT NULL,
    UNIQUE(case_id, fingerprint)
);

CREATE INDEX IF NOT EXISTS idx_tracking_events_case
    ON tracking_events(case_id, occurred_at DESC, created_at DESC);

CREATE TABLE IF NOT EXISTS tracking_refresh_state (
    case_id TEXT PRIMARY KEY NOT NULL REFERENCES rectification_cases(id) ON DELETE CASCADE,
    last_attempt_at TEXT,
    last_success_at TEXT,
    last_error TEXT
);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (4, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
