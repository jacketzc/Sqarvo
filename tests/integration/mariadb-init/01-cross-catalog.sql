-- 跨库表数据用例（#140 / ADR 0024）的第二个库。
-- 测试账号由 MARIADB_USER 创建，入口脚本只给 MARIADB_DATABASE 授权，
-- 因此这里补一个 `<默认库>_b` 并授权：用例在两个库里各建同名表 probe，
-- 验证表数据按 ObjectRef.catalog 取库。
CREATE DATABASE IF NOT EXISTS sqarvo_test_b;
GRANT ALL ON sqarvo_test_b.* TO 'sqarvo_test'@'%';
FLUSH PRIVILEGES;
