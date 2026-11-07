# 集成测试

## 当前状态

MariaDB 驱动集成测试已实现：`backend/crates/drivers/mariadb/tests/integration.rs`
（类型往返、分页游标、取消、catalog = database 的层级语义等）。
PostgreSQL 驱动尚未实现。

用例需要真实数据库：读取环境变量 `SQARVO_MARIADB_URL`。未设置时用例打印
`跳过：未设置 SQARVO_MARIADB_URL` 并直接返回——本地不带该变量跑 `cargo test` 属于预期行为，
但 CI 必须显式设置，且不允许静默跳过（见下文「CI」）。

`docker-compose.yml` 定义了驱动集成测试使用的本地数据库。**不要**连接到
真实生产数据库；只能使用这里定义的、带合成数据的测试实例。

## 运行 MariaDB 集成测试

```bash
cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:sqarvo_test_only@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test integration -- --nocapture
```

先列出用例（不执行，用于核对发现数）：

```bash
cd backend
cargo test -p sqarvo-driver-mariadb --test integration -- --list
```

跨库表数据用例（#140 / ADR 0024）在单独的测试二进制里，需要一个**第二个库**
`<DSN 里的库>_b`（`sqarvo_test` → `sqarvo_test_b`）并给测试账号全权限。
`docker-compose.yml` 通过 `mariadb-init/` 的入口脚本自动建库授权；已经初始化过的实例
需要手工执行 `mariadb-init/01-cross-catalog.sql`：

```bash
cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test cross_catalog -- --nocapture
```

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

## CI

CI 的 `mariadb-tests` 作业（`.github/workflows/ci.yml`）用 GitHub Actions 的
**服务容器**（`mariadb:11.4`，带 `healthcheck.sh --connect --innodb_initialized` 健康检查）
代替本地的 compose / podman，两者使用**同一套账号与同一个环境变量**，只有端口不同：

| 项 | CI（服务容器） | 本地（compose / podman） |
|---|---|---|
| 镜像 | `mariadb:11.4` | `mariadb:11.4` |
| 用户 / 密码 | `sqarvo_test` / `sqarvo_test_only` | 同左 |
| 数据库 | `sqarvo_test` | 同左 |
| 端口 | `3306` | `53306`（避免与本机 3306 冲突） |
| 环境变量 | `SQARVO_MARIADB_URL` | 同左 |

CI 中该变量为：

```text
mysql://sqarvo_test:sqarvo_test_only@127.0.0.1:3306/sqarvo_test
```

这些都是测试专用值，不是真实凭据，因此未使用任何 secret。

作业在跑用例前先做一次端口可达性探测，随后用 `set -o pipefail` + `tee mariadb-tests.log`
保留日志，并按下列规则断言（任一不满足即失败）：

1. 日志中不得出现 `跳过：未设置 SQARVO_MARIADB_URL`；
2. 日志中必须出现 `Running tests/integration.rs`（集成测试二进制确实被执行）；
3. 从 `test result: ok. N passed` 解析出的 `N` 必须大于 0；
4. `cargo test ... -- --list | grep -c ': test'` 得到的「发现数」必须与实际通过数相等，
   防止用例被过滤后仍显示成功。

失败时作业会上传 `backend/mariadb-tests.log` 作为证据。SQLite 覆盖仍由独立的 `backend`
作业在**不启动任何外部服务**的前提下执行，两个作业互不依赖。

## 计划

- MariaDB 11.4 LTS：已实现，见上文；
- PostgreSQL 16：类型往返（numeric、timestamptz、uuid、jsonb、数组）、取消、分页游标；
- SQLite：文件数据库、只读模式、无网络依赖的快速回归。

## 约定

- 测试数据必须可重复生成（种子脚本），不得依赖人工准备；
- 测试账号只拥有测试库权限；
- 连接参数通过环境变量传入，禁止硬编码密码；
- 清理：测试结束后关闭连接池、删除临时表；容器可保留供反复运行。
