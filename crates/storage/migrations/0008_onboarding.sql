CREATE TABLE IF NOT EXISTS app_preferences (
    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK (singleton_id = 1),
    onboarding_completed INTEGER NOT NULL DEFAULT 0 CHECK (onboarding_completed IN (0, 1)),
    onboarding_completed_at TEXT
);

INSERT OR IGNORE INTO app_preferences (
    singleton_id, onboarding_completed, onboarding_completed_at
) VALUES (1, 0, NULL);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (8, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
