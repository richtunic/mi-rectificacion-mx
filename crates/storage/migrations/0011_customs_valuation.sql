CREATE TABLE IF NOT EXISTS case_customs_valuation (
    case_id TEXT PRIMARY KEY NOT NULL,
    presumptive_value_mxn TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    FOREIGN KEY (case_id) REFERENCES rectification_cases(id) ON DELETE CASCADE
);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (11, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
