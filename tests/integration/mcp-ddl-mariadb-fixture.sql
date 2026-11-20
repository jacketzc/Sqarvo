-- Dedicated disposable MariaDB 11.8.6 instance only. Account has no password
-- because the fixture listens on loopback and is discarded after the tests.
CREATE DATABASE sqarvo_mcp_ddl_fixture;
CREATE DATABASE sqarvo_guard;
CREATE DATABASE sqarvo_mcp_ddl_other;
CREATE USER 'mcp_ddl_fixture'@'127.0.0.1';
CREATE USER 'mcp_ddl_read_fixture'@'127.0.0.1';
GRANT SELECT, CREATE, ALTER, DROP, INDEX, INSERT
    ON sqarvo_mcp_ddl_fixture.* TO 'mcp_ddl_fixture'@'127.0.0.1';
GRANT SELECT ON sqarvo_mcp_ddl_fixture.* TO 'mcp_ddl_read_fixture'@'127.0.0.1';

CREATE TABLE sqarvo_mcp_ddl_fixture.locked_table (id INTEGER PRIMARY KEY) ENGINE=InnoDB;
INSERT INTO sqarvo_mcp_ddl_fixture.locked_table VALUES (1);

-- Run as the isolated instance administrator for complete metadata visibility.
CREATE SQL SECURITY DEFINER VIEW sqarvo_guard.ddl_objects AS
SELECT TABLE_SCHEMA AS schema_name, TABLE_NAME AS object_name, TABLE_TYPE AS object_type, ENGINE AS engine
FROM information_schema.TABLES
WHERE TABLE_SCHEMA = 'sqarvo_mcp_ddl_fixture';

-- Keep foreign keys (including outside references), views, triggers, routines
-- and events out of the user schemas for the managed DDL fixture.
CREATE SQL SECURITY DEFINER VIEW sqarvo_guard.ddl_eligibility AS
SELECT 'sqarvo_mcp_ddl_fixture' AS schema_name, 1 AS policy_version,
  NOT EXISTS (SELECT 1 FROM information_schema.INNODB_SYS_FOREIGN)
  AND NOT EXISTS (SELECT 1 FROM information_schema.VIEWS
      WHERE TABLE_SCHEMA NOT IN ('sys', 'mysql', 'information_schema', 'performance_schema', 'sqarvo_guard')
         OR (TABLE_SCHEMA = 'sqarvo_guard' AND TABLE_NAME = 'write_eligibility'))
  AND NOT EXISTS (SELECT 1 FROM information_schema.TRIGGERS
      WHERE TRIGGER_SCHEMA NOT IN ('sys', 'mysql', 'information_schema', 'performance_schema'))
  AND NOT EXISTS (SELECT 1 FROM information_schema.ROUTINES
      WHERE ROUTINE_SCHEMA NOT IN ('sys', 'mysql', 'information_schema', 'performance_schema'))
  AND NOT EXISTS (SELECT 1 FROM information_schema.EVENTS)
  AS eligible;

GRANT SELECT ON sqarvo_guard.ddl_objects TO 'mcp_ddl_fixture'@'127.0.0.1', 'mcp_ddl_read_fixture'@'127.0.0.1';
GRANT SELECT ON sqarvo_guard.ddl_eligibility TO 'mcp_ddl_fixture'@'127.0.0.1', 'mcp_ddl_read_fixture'@'127.0.0.1';
