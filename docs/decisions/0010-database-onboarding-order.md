# ADR 0010：网络数据库接入顺序（MariaDB 优先，PostgreSQL 次之）

- 状态：已接受
- 日期：2026-11-04
- 关联：S3 / M3（MariaDB）、S5 / M5（PostgreSQL）；风险 R-01、R-02

## 背景

v0.1 只有 SQLite 驱动：文件型、无凭据、无网络。第一个网络数据库会首次把凭据存储、
TLS、服务端取消、连接与会话语义落实为真实约束，选错会连带固化后续驱动的假设
（参见 S1「契约冻结」的教训）。因此需要明确数据库顺序，而不是「先接哪个都行」。

候选为 PostgreSQL 与 MariaDB（MySQL 协议族）。两者都能验证 SPI 的第二种实现，
差别在于覆盖的生态、协议特性与类型系统。

## 决策

1. **S3 / v0.2：MariaDB** 为第一个网络数据库，与系统凭据存储一起交付；
2. **S5 / v0.2.4（原记 v1.0，2026-11-05 版本改标）：PostgreSQL** 为第二个网络数据库，
   同时形成三驱动一致性矩阵（SQLite / MariaDB / PostgreSQL）；
3. **MySQL 8.x 不默认声明支持**：MariaDB 驱动基于 MySQL 协议，能否连接 MySQL 8.x
   必须实测后在支持矩阵中如实记录，未验证前 UI 与文档按「不支持」表述。

## 理由

- **生态覆盖**：MariaDB 使用 MySQL 协议与 SQL 方言，一套驱动即可覆盖大量 MySQL 兼容
  部署（含多数面板与云上「MySQL 兼容」实例），单位成本最低；反过来先做 PostgreSQL
  再补 MySQL 族需要第二套协议实现，成本更高。
- **差异足够大**：相对 SQLite，MariaDB 带来网络连接、凭据、catalog = database 的层级、
  无符号整数、`BIT`/`ENUM`/`SET`、服务端取消（`KILL QUERY`）与真实 TLS，
  足以暴露 SPI 设计缺陷，而不是只验证「能连上」。
- **保留跨范式验证**：PostgreSQL 放在第三位，用来验证 database + schema 两级层级、
  原生 schema、numeric/uuid/jsonb/数组与独立 cancel 通道；三个数据库分属文件 / MySQL 族 /
  PostgreSQL 族三种范式，避免「同族两个驱动」造成的一致性矩阵虚假繁荣。
- **顺序可逆成本可控**：若后续业务重心转向 PostgreSQL，MariaDB 已交付的能力不受影响；
  反之把 PostgreSQL 提前则要同时承担两套协议栈。

## 取舍与风险

- MariaDB 没有独立取消通道，必须另开连接执行 `KILL QUERY <connection_id>`；
  取消后原连接能否继续复用、以及事务中取消的行为必须有测试并写入文档（R-04）。
- MariaDB 的 `JSON` 是 `LONGTEXT` 的别名，`TIMESTAMP` 带会话时区语义，零日期
  （`0000-00-00`）在严格模式下可能被拒绝：类型映射必须保留原始类型名，不得伪造
  原生 JSON 或时区类型（R-03）。
- 用户可能把「支持 MariaDB」误读为「支持 MySQL 8」：支持矩阵与连接对话框必须显式区分，
  未实测前不提供 MySQL 8 的驱动条目。
- 后端 SQLite 之外的实现细节（认证插件 `ed25519`、`unix_socket`、字符集与排序规则）
  需要具体驱动库支持，选型在 S3 的驱动选型 ADR 中单独决定，本 ADR 只定数据库顺序。

## 重新评估条件

- 目标用户与部署环境以 PostgreSQL 或 MySQL 8 为主（发布后使用数据或明确的客户需求）；
- MariaDB 驱动遇到协议层无法绕过的差异（认证插件、连接属性、复制相关语义）；
- 需要覆盖 MySQL 8 独有特性（如 `CHECK` 强制、窗口函数差异、`caching_sha2_password`）；
- 云厂商托管实例只提供 MySQL 8 协议变体，且兼容性实测失败。
