-- Public forwarding addresses are local presentation metadata. The canonical
-- addresses.address, route_id and alias_id remain the Ahrara routing identity.
ALTER TABLE addresses ADD COLUMN public_address TEXT COLLATE BINARY;
ALTER TABLE addresses ADD COLUMN forwarding_address TEXT COLLATE BINARY;
CREATE UNIQUE INDEX addresses_public_address ON addresses(public_address);
CREATE TABLE custom_domains (
    identity_fingerprint TEXT PRIMARY KEY,
    domain TEXT NOT NULL,
    forwarding_address TEXT NOT NULL COLLATE BINARY
);
