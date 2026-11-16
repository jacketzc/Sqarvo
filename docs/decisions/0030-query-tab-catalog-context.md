# ADR 0030：查询标签的库上下文（显式 catalog + 会话切库）

- 状态：已接受（2026-11-16，维护者确认改动确认单 #3 / #4）
- 日期：2026-11-16
- 关联：AGENTS.md §7（gRPC / Protobuf）、§8.6（UI 规范）、§14（文档与 ADR）、§15.0（改动确认单）；
  [ADR 0024](0024-cross-catalog-table-access.md)（跨库表数据按 `ObjectRef.catalog` 定位）、
  [ADR 0019](0019-v022-metadata-ddl-and-insert.md) §「跨库 / 跨 schema 引用必须显式」、
  `docs/api/grpc-conventions.md`「服务端不得隐式切到连接默认库」
- 上游：Issue #182（无默认库连接上未限定表名的 SQL 只报 `mariadb.server_error`）

## 背景

会话的库归属今天只有一个来源：连接配置里的 `database`。

- 它为空时，会话在服务端没有默认库，任何未限定表名的 SQL 都会被服务端拒绝（1046
  `No database selected`）。#182 之前这条失败还被归类成笼统的 `mariadb.server_error`。
- 前端没有任何接口能问出「这条会话现在到底落在哪个库」：查询标签 tooltip 的「库 默认」
  与信息面板的「默认库」都取自配置值，不是服务端真值。
- 用户没有任何入口改这件事：要么重开一条连接、要么在 SQL 里手打 `库.表`。

同时仓库里已经有一条明确的口径（`grpc-conventions.md`、ADR 0024）：**跨库引用必须显式，
服务端不得隐式切到连接默认库**。这条口径针对的是「请求没给出库归属时，服务端不要替客户端做主」，
它并不禁止「客户端显式要求这条会话使用某个库」。

维护者要求：读上下文 + 用户可视化修改「库」，并让 `Execute` 带库归属。

## 备选方案

1. **驱动改写 SQL 里的表名补限定**：不可行。多语句、子查询、CTE、`JOIN`、别名、函数体都要解析，
   等于在驱动里造一个 SQL 解析器，且用户自己写的限定名会被二次改写。
2. **前端把库写进 SQL 文本**（执行前拼 `USE db;`）：把产品语义塞进用户文本——多语句策略、取消、
   事务边界、语句摘要与导出都会受影响，用户也看不见自己「被改过」的 SQL。
3. **每个标签一个会话 + 显式 `catalog` + 会话内 `USE`**（本 ADR 采用）：每条标签本来就有自己的
   会话（`tabs.query` 组件口径），切库只影响这一条会话，不会串到别的标签。
4. **只做只读上下文，不改执行语义**：解决不了根本问题（未限定表名仍然失败），只是把猜改成看。
   #182 里已记录为遗留项，本 ADR 取代它。

## 决策

1. **`ExecuteQueryRequest` 追加 `string catalog = 6`**（只追加，不复用字段号）。
   - 空串 = **不改变**会话当前库，完全等于既有行为：旧客户端行为不变，「不隐式切库」仍然成立；
   - 非空 = 执行前把这条会话切到该库。这是**用户的显式意图**，不是服务端替客户端做主。
   这条也澄清了 `grpc-conventions.md` 那句：不得在请求未给出时替客户端选库；请求给出了就照办。
2. **切库是会话级动作，不是语句级改写**。SPI 新增 `DriverSession::use_catalog(&str)`，
   默认实现返回 `capability.unsupported`。通用层在**同一次会话执行锁**里先切库再执行
   （`SessionHandle::execute_in_catalog`），中间不可能插入别人的语句。
3. **MariaDB 用 `USE \`db\``**；SQLite 没有「切当前库」这个概念（`main` / `temp` / `ATTACH`
   是 catalog 列表），`session_catalog_switch = false`，带非空 `catalog` 时返回
   `capability.unsupported`，**不静默忽略**。
4. **驱动缓存会话当前库**：连接时读一次 `SELECT DATABASE()`；自己发过 `USE` 就更新缓存；
   用户语句首关键字是 `use` 时把缓存置为「未知」，下次需要比较时先重读再决定。缓存只用于
   「跳过重复 `USE`」，任何一次不确定都退回重读——宁多一次往返，不可落错库。
5. **事务打开时不切库**：收到与会话当前库不同的 `catalog` 返回 `FailedPrecondition`
   （`mariadb.catalog_switch_in_transaction`），文案「事务已打开：先提交或回滚，再切换库」。
   不静默忽略，也不隐式提交用户事务。
6. **切库失败不回落**：1049 → `mariadb.unknown_database`、1044 / 1142 / 1143 →
   `mariadb.permission_denied`，语句根本不发送。绝不能「切库失败就按旧库跑」。
7. **真值只有一个来源**：新增 `ConnectionService.GetSessionContext(session_id)`，返回服务端
   实际生效的库（可空）与这一层叫什么（`CATALOG_KIND_DATABASE` / `CATALOG_KIND_SCHEMA`）。
   前端只显示这个值，不拿配置值顶替。
8. **显示与请求分离**：刷新上下文只更新「显示」，不自动改写标签请求用的 `catalog`。否则用户
   自己在 SQL 里写的 `USE` 会把标签状态来回拉扯。
9. **库名不进状态栏**：v0.2.3.3 已定「状态栏不放库名」，本 ADR 不改这条；库显示在标签 tooltip、
   查询标签动作行的库选择器（`tabs.query.database_picker`）与 `inspector.catalog`。
10. **`ConnectionService.SetSessionContext(session_id, catalog)` 与 `Execute.catalog` 是同一能力
    的两个入口，不是第二套语义**。两者都只落到驱动的 `use_catalog`：幂等、事务中拒绝、
    失败不回落、不支持切库的驱动返回 `capability.unsupported`，逐条相同。差别只在入口形态——
    `Execute.catalog` 是「切库 + 执行」一次往返完成，`SetSessionContext` 是「只切库」并返回切后
    上下文（所以客户端不必再问一次 `GetSessionContext`）。空串在两条入口上都表示「不改变」。
    之所以要有这条 RPC：用户在下拉框里改库时**还没打算执行 SQL**，此时不该为了一个 UI 状态去
    编一条假语句，也不该让前端先把库记下来、等执行时才暴露失败。

## 代价与权衡

- **一次往返**：带 `catalog` 且与缓存不同时多一条 `USE`；相同时为 0。缓存被置未知时多一条
  `SELECT DATABASE()`。极端情况（每条语句前都手写 `USE`）每次多两条轻量语句。
- **事务中不能切库**：这是有意限制。真要支持，需要把库归属下推到每条语句（回到备选 1 的
  SQL 解析问题），代价远高于收益。
- **SQLite 无切库**：选择器在该驱动上隐藏，用户看到的是 `main`；这符合 SQLite 的语义，不是缺陷。
- **与 ADR 0024 的关系**：0024 的「`catalog` 缺省回落会话默认库」完全不变；本 ADR 只新增
  「客户端可以显式设定会话当前库」。表数据 / 行编辑 / 元数据的库归属仍按 `ObjectRef.catalog`
  逐请求表达，不读会话当前库做隐式推断。

## 验收证据（本 ADR 对应的实现）

| 断言 | 用例 |
|---|---|
| 带 `catalog` 能读到目标库的表（两库同名表行数不同） | `mariadb/tests/cross_catalog.rs` 扩展用例 |
| `catalog` 为空时仍未限定表名的 SQL 报 `mariadb.no_database` | 既有 #182 用例 |
| 事务中带不同 `catalog` → `mariadb.catalog_switch_in_transaction` | `mariadb/tests/integration.rs` |
| 无权限 / 不存在的库 → `permission_denied` / `unknown_database`，语句未发送 | `mariadb/tests/integration.rs` |
| SQLite 带 `catalog` → `capability.unsupported` | `sqlite/tests/cross_catalog.rs` |
| `GetSessionContext` 返回服务端真值；未知会话 → `session.not_found` | `rpc-server/tests/grpc_smoke.rs` |
| `SetSessionContext` 在 SQLite 上 → `capability.unsupported`；空 `catalog` 成功并返回 `main` | `rpc-server/tests/grpc_sqlite_e2e.rs` |

## 重估条件

- **接入 PostgreSQL 时**：`catalog_kind` 需要取 `CATALOG_KIND_SCHEMA`，切库实现为
  `SET search_path`，`session_catalog_switch` 仍可为 true——届时补一条驱动级用例，不改契约。
- **出现「事务中必须切库」的真实需求**时：回到把库归属下推到语句的路线，本 ADR 第 5 条作废。
- **远程后端 / 多客户端共享一条会话**成为现实后：会话级切库的假设（一条会话只服务一个标签）
  需要重新评估，改为每条语句携带库归属。