-- Run once in an existing MSSQL database, then grant your team SELECT and UPDATE.
-- Relay never creates a database or modifies other tables.
IF OBJECT_ID(N'dbo.RelayLibrary', N'U') IS NULL
BEGIN
    CREATE TABLE dbo.RelayLibrary (
        Id INT NOT NULL CONSTRAINT PK_RelayLibrary PRIMARY KEY,
        Revision BIGINT NOT NULL,
        Payload NVARCHAR(MAX) NOT NULL,
        CONSTRAINT CK_RelayLibrary_Singleton CHECK (Id = 1),
        CONSTRAINT CK_RelayLibrary_Json CHECK (ISJSON(Payload) = 1)
    );
END;
IF NOT EXISTS (SELECT 1 FROM dbo.RelayLibrary WHERE Id = 1)
    INSERT INTO dbo.RelayLibrary (Id, Revision, Payload) VALUES (1, 0, N'[]');

-- Example (replace RelayTeam with your existing database user or role):
-- GRANT SELECT, UPDATE ON dbo.RelayLibrary TO RelayTeam;
