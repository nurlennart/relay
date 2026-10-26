-- Run once in an existing MySQL 8.0.16+ or MariaDB 10.4+ database, then grant your team SELECT
-- and UPDATE. Relay never creates a database or modifies other tables.
CREATE TABLE IF NOT EXISTS RelayLibrary (
    Id INT NOT NULL PRIMARY KEY,
    Revision BIGINT NOT NULL,
    Payload LONGTEXT NOT NULL,
    CONSTRAINT CK_RelayLibrary_Singleton CHECK (Id = 1),
    CONSTRAINT CK_RelayLibrary_Json CHECK (JSON_VALID(Payload))
) DEFAULT CHARACTER SET utf8mb4 COLLATE utf8mb4_bin;
INSERT IGNORE INTO RelayLibrary (Id, Revision, Payload) VALUES (1, 0, '[]');

-- Example (replace the database, user, and host with your own):
-- GRANT SELECT, UPDATE ON relay.RelayLibrary TO 'relay_team'@'%';
