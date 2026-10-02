CREATE TABLE messages (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    transfer_id TEXT NOT NULL,
    identity_fingerprint TEXT NOT NULL,
    recipient TEXT NOT NULL COLLATE BINARY,
    envelope_sender TEXT NOT NULL,
    received_at TEXT NOT NULL,
    from_header TEXT NOT NULL,
    to_header TEXT NOT NULL,
    subject TEXT NOT NULL,
    message_id TEXT,
    size_bytes INTEGER NOT NULL,
    raw_eml BLOB NOT NULL,
    UNIQUE(transfer_id, recipient)
);
CREATE INDEX messages_recipient ON messages(recipient);
CREATE INDEX messages_received_at ON messages(received_at);
