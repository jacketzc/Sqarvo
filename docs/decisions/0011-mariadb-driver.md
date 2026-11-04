# ADR 0011：MariaDB 驱动库选型与取消语义

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3、#18（本 ADR）、#20、#22；ADR 0005（游标所有权）、ADR 0007（截断）、ADR 0010（接入顺序）

## 背景

MariaDB 是第一个网络数据库（ADR 0010）。驱动库决定了四件很难后改的事：连接与 TLS 参数的形状、
结果集读取方式（能否按批交付）、取消能否传播到服务端、以及类型映射的可控程度。

SPI 要求（`docs/database/driver-spi.md`）：游标由驱动持有、`fetch(max_rows)` 分批、
`cancel` 幂等且不经过执行锁、阻塞调用不得占用 Tokio worker。

## 候选

| 方案 | 版本 / 许可证 | 评估结论 |
|---|---|---|
| `mysql_async` | 0.37.1，MIT OR Apache-2.0 | **采用**。纯 Rust（协议实现 `mysql_common`），Tokio 异步，行级流式读取，rustls/ring/aws-lc-rs 可选 TLS，含 `client_ed25519`、`client_parsec`；MariaDB 官方文档推荐社区 crate 之一并含 MariaDB 侧贡献的特性 |
| `sqlx` | 0.9.0，MIT OR Apache-2.0 | 不采用（本阶段）。统一 API 便于 S5 的 PostgreSQL 复用，但查询宏带来构建成本，游标与取消的可控性不如直接使用协议层；留作 S5 候选 |
| MariaDB Connector/Rust | crates.io 无可用的维护版本（`cargo info mariadb` 未找到） | 不采用。需要系统 C 客户端库，跨平台打包成本高 |

## 实测（2026-11-04，Linux，本地容器）

环境：`docker.io/library/mariadb:11.4`，服务端 `11.4.13-MariaDB-ubu2404`，127.0.0.1:53306，
一次性验证程序位于仓库外的 scratch 目录（不进入提交）。结论：

| 项目 | 结果 |
|---|---|
| 连接 / 版本探测 | 成功；`SELECT VERSION()`、`CONNECTION_ID()` 可用 |
| TLS | `SslOpts::default().with_danger_accept_invalid_certs(true)` 握手成功，`Ssl_version = TLSv1.3`（容器自签证书；生产必须校验） |
| 默认认证插件 | 容器默认 `mysql_native_password`；`ed25519` / `parsec` 需服务端启用与额外特性，未实测 |
| 值表示 | 文本协议下所有列以 `Value::Bytes` 返回（含 `BIGINT UNSIGNED`、`DECIMAL`、`DATETIME(3)`、`TIME(6)`、`BIT(8)`、`ENUM`、`SET`、`JSON`、`BLOB`、`YEAR`），**必须按列类型解析**，不能按 Rust 值类型猜测 |
| 零日期 | 容器默认 `sql_mode` 非严格，`0000-00-00 00:00:00` 写入成功 → 必须作为可出现的值显式处理 |
| `JSON` 列 | 服务端把该列存为 `longtext`：`information_schema.columns.DATA_TYPE = longtext`，`SHOW CREATE TABLE` 为 ``longtext ... CHECK (json_valid(`js`))`` → 只能作为带校验的文本处理，与 ADR 0010 的判断一致 |
| 流式读取 | 10 万行（无索引全表）经 `QueryResult::stream::<Row>()` 读取耗时约 180 ms；**没有分批 API**，只有行流 |
| 取消 | 协议无独立取消通道；另一连接执行 `KILL QUERY <id>` 后 0.2–0.3 ms 内返回 `ERROR 1317 (70100): Query execution was interrupted` |
| 取消后连接复用 | 被中断的连接与被用于 `KILL` 的连接均可继续执行 `SELECT 1` |
| 连接池退出 | `Pool::disconnect()` 会等待所有 `Conn` 归还；未 `drop` 的连接会让它一直挂住 |

## 决策

1. 采用 `mysql_async 0.37`，关闭默认特性，启用 `default-rustls-ring`（避免 `aws-lc-rs` 的 cmake 构建依赖）；
   压缩特性默认关闭（减少构建与依赖面）。
2. 每会话一个 `Conn`；`fetch` 由驱动持有的 `RowStream` 累积到 `max_rows` 再交付，
   与 ADR 0005 的游标所有权一致，不使用 `OFFSET` 重查。
3. 取消：驱动额外持有一个「控制连接」，`cancel` 从其发出 `KILL QUERY <connection_id>`；
   会话连接的 `connection_id` 在连接建立后读取并缓存。取消幂等：对已结束的查询重复取消返回成功。
4. TLS 模式映射：`disable → 无 ssl_opts`、`prefer → ssl_opts(不强制)`、`require → ssl_opts`、
   `verify-ca`/`verify-full → 严格校验根证书与主机名`；`danger_accept_invalid_certs` 仅用于测试。
5. 错误映射：`ERROR 1317/70100` → `query.cancelled`；`ERROR 1045` → 认证失败；
   `ERROR 2002/2013` → 断线；连接超时 → 超时。驱动错误不得携带密码或完整连接串。

## 取舍

- 无分批 API：每批最多在内存中持有 `max_rows` 行，而不是整表；这是可接受的代价。
- 文本协议全为字节：类型映射工作量上升，但正好满足「保留原始类型名、不伪造类型」的要求。
- 控制连接使每个会话占用两条服务端连接；会话数上限与资源清理需在 `close` 中一并释放。
- `mysql_async` 的 `Pool` 不作为业务层抽象：会话仍由 `SessionManager` 管理，避免连接池语义泄漏到 SPI。

## 重新评估条件

- S5 引入 PostgreSQL 时，若两套驱动的类型与错误映射出现重复实现，评估统一到 `sqlx`；
- 需要 `bulk execute`、压缩或 `LOAD DATA LOCAL INFILE`（导入导出阶段）；
- MySQL 8.x 兼容性（`caching_sha2_password`）需要声明支持时，先补实测；
- 官方 Connector/Rust 提供维护中的 crate 与跨平台打包方案。
