-- Load as the admin of a disposable integration fixture with the two test catalogs.
CREATE TABLE sqarvo_test.s6_orders (id INT PRIMARY KEY, owner VARCHAR(20));
CREATE TABLE sqarvo_test_b.s6_orders (id INT PRIMARY KEY, owner VARCHAR(20));
INSERT INTO sqarvo_test.s6_orders VALUES (1, 'primary-fixture');
INSERT INTO sqarvo_test_b.s6_orders VALUES (1, 'peer-fixture');

CREATE DATABASE sqarvo_s8_denied;
CREATE TABLE sqarvo_s8_denied.denied (id INT);
-- List the catalog name without granting access to its contents or switching into it.
CREATE USER 'sqarvo_ui_scope'@'%' IDENTIFIED BY 'sqarvo_ui_scope_test_only';
GRANT SHOW DATABASES ON *.* TO 'sqarvo_ui_scope'@'%';
GRANT SELECT ON sqarvo_test.* TO 'sqarvo_ui_scope'@'%';
GRANT SELECT ON sqarvo_test_b.* TO 'sqarvo_ui_scope'@'%';
