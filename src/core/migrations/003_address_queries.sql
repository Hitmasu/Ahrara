-- Ordered pagination and covering counts without scanning/sorting all aliases.
CREATE INDEX addresses_identity_id ON addresses(identity_fingerprint, id);
CREATE INDEX addresses_identity_enabled_id ON addresses(identity_fingerprint, enabled, id);
