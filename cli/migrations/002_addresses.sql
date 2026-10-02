CREATE TABLE addresses (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    identity_fingerprint TEXT NOT NULL,
    route_id BLOB NOT NULL CHECK(length(route_id) = 16),
    alias_id BLOB NOT NULL CHECK(length(alias_id) = 8),
    address TEXT NOT NULL COLLATE BINARY UNIQUE,
    label TEXT,
    created_at TEXT NOT NULL,
    enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),
    UNIQUE(identity_fingerprint, alias_id)
);
CREATE INDEX addresses_alias_id ON addresses(alias_id);
