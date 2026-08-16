PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS product_lines (
    id TEXT PRIMARY KEY NOT NULL,
    case_id TEXT NOT NULL REFERENCES rectification_cases(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    seller TEXT,
    quantity INTEGER NOT NULL CHECK (quantity > 0),
    unit_price TEXT NOT NULL,
    discount TEXT NOT NULL,
    shipping TEXT NOT NULL,
    taxes TEXT NOT NULL,
    currency TEXT NOT NULL,
    subtotal_original TEXT NOT NULL,
    total_original TEXT NOT NULL,
    total_mxn TEXT NOT NULL,
    rate_date TEXT NOT NULL,
    rate_to_mxn TEXT NOT NULL,
    rate_source_name TEXT NOT NULL,
    rate_source_url TEXT NOT NULL,
    rate_fetched_at TEXT NOT NULL,
    rate_is_manual INTEGER NOT NULL CHECK (rate_is_manual IN (0, 1)),
    manual_rate_reason TEXT,
    created_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_products_case_created
    ON product_lines(case_id, created_at);

INSERT OR IGNORE INTO schema_migrations(version, applied_at)
VALUES (3, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'));
