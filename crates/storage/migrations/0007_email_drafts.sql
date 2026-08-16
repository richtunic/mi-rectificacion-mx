CREATE TABLE IF NOT EXISTS email_drafts (
    case_id TEXT PRIMARY KEY NOT NULL REFERENCES rectification_cases(id) ON DELETE CASCADE,
    recipient TEXT NOT NULL,
    sender TEXT NOT NULL,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    request_pdf_path TEXT NOT NULL,
    evidence_pdf_path TEXT NOT NULL,
    eml_path TEXT NOT NULL,
    prepared_at TEXT NOT NULL,
    opened_at TEXT,
    sent_at TEXT
);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (7, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
