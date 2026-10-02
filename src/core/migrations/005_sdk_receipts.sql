CREATE TABLE sdk_receipts (
    message_id INTEGER PRIMARY KEY
);
CREATE TRIGGER sdk_receipts_delete AFTER DELETE ON messages
BEGIN
    DELETE FROM sdk_receipts WHERE message_id = OLD.id;
END;
