CREATE TABLE IF NOT EXISTS case_document_notes (
    case_id TEXT PRIMARY KEY NOT NULL,
    request_notes TEXT NOT NULL DEFAULT '',
    updated_at TEXT NOT NULL,
    FOREIGN KEY (case_id) REFERENCES rectification_cases(id) ON DELETE CASCADE
);
