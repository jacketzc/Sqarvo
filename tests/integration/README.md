# 集成测试

## 当前状态

尚未实现数据库驱动，因此没有可运行的数据库集成测试。

`docker-compose.yml` 定义了将来驱动集成测试使用的本地数据库。**不要**连接到
真实生产数据库；只能使用这里定义的、带合成数据的测试实例。

## 计划

- MariaDB 11.4 LTS：类型往返（无符号整数、`DECIMAL`、`BIT`、`ENUM`/`SET`、`JSON` 别名、零日期）、
  取消（`KILL QUERY`）、分页游标、catalog = database 的层级语义；
- PostgreSQL 16：类型往返（numeric、timestamptz、uuid、jsonb、数组）、取消、分页游标；
- SQLite：文件数据库、只读模式、无网络依赖的快速回归。

## 启动测试数据库

```bash
cd tests/integration
docker compose up -d
```

没有 docker 权限时可用 rootless podman（本机已验证，2026-11-04）：

```bash
podman run -d --name sqarvo-mariadb-test \
  -e MARIADB_USER=sqarvo_test -e MARIADB_PASSWORD=sqarvo_test_only \
  -e MARIADB_DATABASE=sqarvo_test -e MARIADB_ROOT_PASSWORD=sqarvo_root_test_only \
  -p 127.0.0.1:53306:3306 docker.io/library/mariadb:11.4
```

镜像仓库不可直连时，podman 需要设置 `HTTPS_PROXY`（例如 `http://127.0.0.1:7890`），
或改用可用的镜像源。SQLite 用例不依赖任何服务，始终可运行。

## 约定

- 测试数据必须可重复生成（种子脚本），不得依赖人工准备；
- 测试账号只拥有测试库权限；
- 连接参数通过环境变量传入，禁止硬编码密码；
- 清理：测试结束后关闭连接池、删除临时表；容器可保留供反复运行。
