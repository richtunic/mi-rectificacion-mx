CREATE TABLE IF NOT EXISTS applicant_profile (
    singleton_id INTEGER PRIMARY KEY NOT NULL CHECK (singleton_id = 1),
    full_name TEXT NOT NULL DEFAULT '',
    email TEXT NOT NULL DEFAULT '',
    phone TEXT NOT NULL DEFAULT '',
    address TEXT NOT NULL DEFAULT '',
    city TEXT NOT NULL DEFAULT '',
    state TEXT NOT NULL DEFAULT '',
    postal_code TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL
);

INSERT OR IGNORE INTO applicant_profile (
    singleton_id, full_name, email, phone, address, city, state, postal_code, updated_at
) VALUES (1, '', '', '', '', '', '', '', strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (6, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
