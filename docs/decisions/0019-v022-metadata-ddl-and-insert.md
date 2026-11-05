# ADR 0019：v0.2.2 元数据层级、DDL 与插入行契约

- 状态：已接受
- 日期：2026-11-05
- 关联：Issue #81；REQ-META-001/003/004、REQ-EDIT-002；
  ADR 0005（游标）、ADR 0007（截断）、ADR 0008（会话执行锁）、ADR 0017（表数据与行编辑）、
  ADR 0018（取消）；后续实现：#82（对象树与搜索）、#83（插入行）、#84（DDL）

## 背景

v0.2.2 的四件事（对象树、DDL 查看、对象搜索、插入新行）都会动契约。v0.2.1 的教训是：
契约一改，两个驱动 + rpc-server + 三个前端同时连带改。所以本版先冻结契约、只做增量，
实现留给 #82–#84。

四个必须一次说清的问题：

1. **对象层级**：Catalog / Schema / Index / Sequence 怎么被寻址，根是什么，跨库怎么表达；
2. **搜索放在哪一层**：内存过滤还是下推到数据库，条件怎么进分页 token；
3. **DDL 的可信度**：只有 `text` + `exact` 布尔时，用户无法判断「不精确」是不精确在哪；
4. **插入行**：省略列与显式 NULL 怎么区分，自增主键怎么回传，一批插入失败时库里留下什么。

## 备选方案

### 1. 对象层级

1. **压平成「一个对象列表 + 点分路径字符串」**：前端自己按 `.` 切分出层次。
   - 优点：一个 RPC、一个列表。
   - 缺点：库名/表名里含 `.` 时无法解析；索引、触发器挂在哪张表上没有表达；名字变长就破契约。
     **否决**。
2. **为每层一个 RPC**（`ListCatalogs` / `ListSchemas` / `ListTables` / `ListIndexes`）。
   - 优点：每层的参数与返回类型可以专门化。
   - 缺点：RPC 数量随数据库层级膨胀（PostgreSQL 有 catalog+schema，MySQL 只有 catalog，
     Oracle 还有 user/synonym）；新增一层就是一次破坏性协议变更。**否决**。
3. **保留 `ObjectKind` + `ObjectRef(catalog, schema, name)` 的通用寻址**，把「父节点种类 →
   子节点种类」的展开规则写进契约（本决策）。**采用**。

### 2. 搜索

1. **内存过滤**：先 `list_children` 拿全量，再在服务端/驱动里按名字筛。
   - 优点：驱动零改动，一处实现两个驱动共用。
   - 缺点：（a）直接违反 ADR 0005/0017 的「禁止一次拉整库/整表进内存」；（b）十万对象的库每次
     搜索都全量读一遍；（c）**分页语义崩掉**——过滤后的集合大小不同，token 里的 offset 解释不了，
     要么重算（等于每次翻页重扫），要么接受「翻页结果不稳定」；（d）驱动会「看起来实现了搜索」，
     实际没有下推，将来真下推时行为会变（大小写、转义规则），前端无从预期。**否决**。
2. **下推到数据库 + 显式转义 + 条件进 token 指纹**：SQLite `LIKE ... ESCAPE`，
   MariaDB `information_schema` 的 `LIKE`；`%`/`_` 由驱动转义成字面量；过滤条件的规范化字符串
   折进分页 token 的指纹，条件一变旧 token 就失效。**采用**。
3. **全库全文/倒排索引**：能力最强，但要维护索引与失效策略，且 SQLite/MariaDB 的 FTS 语义差异大。
   超出 v0.2.2，**否决**（留给后续版本，届时是独立 ADR）。

### 3. DDL

1. **只保留 `text` + `exact` 布尔（v0.2.1 现状）**：`exact = false` 只告诉用户「这不是原文」，
   不告诉他是我们拼的、拼的时候缺了什么、哪个版本拼的。用户无法判断「这份 DDL 能不能信」。
   作为终态**否决**（字段保留，不删）。
2. **加 `DdlSource` 枚举 + `source_detail` + `generator_version`**：原文路径必须说明取自
   `sqlite_master.sql` 还是 `SHOW CREATE TABLE`；拼装路径必须说明缺什么、生成器版本是多少。
   **采用**。
3. **只返回拼装结果、不区分来源**：最省事，也最危险——用户会把拼装结果当权威 DDL 去建库。
   **否决**。

### 4. 插入行

1. **单行 RPC + 批量 RPC 两套**：前端要按行数分支，风险阈值与原子性必然在两条路径上分叉。
   **否决**（单行 = `rows` 只有一条）。
2. **复用 `ApplyRowChanges`**（把 INSERT 当成「没有主键的 update」）：主键未知、无法做影响行数
   核对、生成的键无处回传。**否决**。
3. **新 `InsertRows` + `InsertRow` / `InsertRowResult`**。**采用**。

子决策：

- **「省略列」怎么表达**：用「该列不在 `values` 里」表达，而不是给 `ColumnValue` 加一个
  `DEFAULT` 变体。理由：默认值（自增、`now()`、`'0'`）只有数据库能求值；把 `DEFAULT` 塞进
  值模型会污染结果集路径（结果集里的 `DEFAULT` 是什么意思？），而且和 `ColumnDefault` 四态
  重复表达同一件事。被否方案：`kind = COLUMN_DEFAULT_EXPRESSION` 直接当写入值——那是**未求值
  的 SQL 文本**，写进 INSERT 就是把字符串拼进 SQL。
- **主键回传**：必须回传插入后的完整主键值（自增取 `last_insert_rowid` / `LAST_INSERT_ID`，
  复合主键回传全部列）。被否方案「不回传，前端整表重刷」的代价：用户看不到刚插入的那行，
  大表上重刷一页还可能因为排序/游标而不含新行；且新行无法立即继续编辑。
- **原子性**：整批成功才提交（自动提交 → 一个隐式事务；事务模式 → `SAVEPOINT`）。
  被否方案 A「逐行独立提交」：部分成功留在库里，用户以为整批要回滚，实际已经有了半批数据，
  与「批量导入」的预期相反。被否方案 B「遇错立即中止」：用户要反复试错才能把冲突找齐。
  本决策要求**继续执行完剩下的行**以便一次报全失败，然后整批回滚。
- **取消**：本版**不提供**。理由：插入批次通常比更新批次快得多（无主键定位、无 WHERE 匹配），
  而取消通道要求 `query_id` + 驱动在行间观察令牌（ADR 0018 的那套机器）。字段可后续追加，
  不需要改既有字段号。代价：极大批次只能等（前端已有 500 行钳制，等待上限可预期）。

## 决策

### 1. 层级与寻址（`proto/services/v1/metadata.proto`）

- `ObjectRef` 不变（`session_id` / `catalog` / `schema` / `name` / `kind`），层级由
  `catalog` → `schema` → `name` 三段 + `kind` 表达；不适用的层级留空，**不得**用空串冒充
  「这一层存在但名字为空」；
- 根 = `parent` 为空（既有行为：当前连接库的表/视图/序列）；
- 展开规则（`ObjectKind` 注释里的表）：CATALOG → SCHEMA 或（无 schema 层的库）TABLE/VIEW/SEQUENCE；
  SCHEMA → TABLE/VIEW/SEQUENCE/FUNCTION/PROCEDURE/TYPE；TABLE → COLUMN/INDEX/TRIGGER；
  VIEW → COLUMN；其余为叶子；
- **跨库/跨 schema 引用必须显式**：缺省层级不得被替换成连接默认库（不隐式切库）；
- 未实现的层级 → `capability.unsupported`（`UNIMPLEMENTED`），**不返回空列表**；
  空列表只表示「这一层确实没有对象」；
- 种类不适用于该操作 → `metadata.object_kind_unsupported`（`INVALID_ARGUMENT`），与
  「驱动还没实现这一层」严格区分。

### 2. 过滤（`ListNodesRequest.filter`，新增字段 3）

```proto
message ObjectNameFilter { oneof match { string contains = 1; string exact = 2; } }
message NodeFilter { ObjectNameFilter name = 1; repeated ObjectKind kinds = 2; }
```

- 名称与种类取**交集**；两者都缺省 = 不过滤；
- 下推：SQLite `LIKE ... ESCAPE`、MariaDB `information_schema` 的 `LIKE`；
- `%` / `_` 按字面量匹配（驱动转义），客户端不拼通配符；
- **大小写敏感性跟随数据库**，契约不提供开关（见「代价」）；
- 过滤条件参与 token 指纹：
  规范化字符串 = `name=<contains|exact>:<原文>` + `kinds=<按声明顺序排序的种类>`，
  由 `ObjectFilter::fingerprint_input()` 产出；条件变化 → 旧 token 以 `config.invalid` 失效；
- 非法条件（`contains` 与 `exact` 同时给、`kinds` 重复或含 `UNSPECIFIED`、空匹配串）→
  `metadata.filter_invalid`（`INVALID_ARGUMENT`）；
- **驱动未实现下推时 rpc-server 显式拒绝非空过滤**，不静默返回未过滤列表。

### 3. DDL（`GetDdlResponse` 追加字段 3–5）

| 字段 | 含义 |
|---|---|
| `source`（`DdlSource`） | `DATABASE` / `SYNTHESIZED` / `UNSPECIFIED`（未说明） |
| `source_detail` | 原文说明取自哪里；拼装说明缺什么。`exact = false` 时**必须**非空 |
| `generator_version` | 拼装器版本；仅 `SYNTHESIZED` 有意义，且必须非空 |

- 不变式：`exact == (source == DDL_SOURCE_DATABASE)`；SPI 侧由
  `Ddl::is_consistent()` 断言，rpc-server 在返回前检查，不自洽就报 `internal.error`；
- SQLite 用 `sqlite_master.sql`（`exact = true`）；MariaDB 用 `SHOW CREATE TABLE/VIEW`
  （`exact = true`）；只有数据库给不出原文时才拼装，且必须 `exact = false` + 说明；
- `GetDdl` 纳入会话执行锁（ADR 0008）。

### 4. 插入行（`table_data.proto`）

- RPC：`TableDataService.InsertRows(InsertRowsRequest) → InsertRowsResponse`，**追加在末尾**；
- 请求：`session_id` / `object` / `transaction_id` / `rows` / `acknowledge_risk`；
- `InsertRow.values` 复用 `ColumnChange`（列 + 强类型值），**省略的列交给数据库默认值**，
  `VALUE_KIND_NULL` 才是写 NULL。「该列有没有默认值」由 `ColumnMeta.default_value` 四态表达，
  服务端不替用户把「省略」改写成默认值字面量；
- 响应：逐行 `InsertRowResult{row_index, outcome, key, rows_affected, error_code, message}` +
  `total_rows_affected`（净影响，回滚时恒 0）+ `clamped` / `deferred_rows` +
  `rolled_back` + `inserted_rows`；
- `key` 回传插入后的**完整**主键值；驱动拿不到完整主键就**不能**声明支持插入；
- 风险阈值沿用 `acknowledge_risk` 与 `DEFAULT_RISK_CONFIRMATION_ROWS = 100`，
  预计影响行数 = 本批行数（插入恒 1 行/行），不另立阈值；
- 单批上限 500（`MAX_ROWS_PER_INSERT_BATCH`，与行变更同值），超限钳制 + `deferred_rows`；
- SPI（全部有默认实现，既有驱动零改动）：

```rust
pub trait TableDataProvider {
    fn supports_row_insertion(&self) -> bool { false }
    async fn insert_rows(&self, _request: InsertRowsRequest) -> Result<InsertReport> {
        Err(unsupported_row_insertion())      // capability.unsupported
    }
}

pub trait MetadataProvider {
    async fn list_children_filtered(
        &self, _parent: Option<ObjectRef>, _filter: &ObjectFilter, _page: &PageRequest,
    ) -> Result<Page<ObjectNode>> {
        Err(unsupported_metadata_filtering()) // capability.unsupported
    }
}
```

### 5. 新增错误码

| 错误码 | gRPC | 场景 |
|---|---|---|
| `table_data.unique_violation` | `ALREADY_EXISTS` | 唯一约束/主键重复（逐行） |
| `table_data.not_null_violation` | `INVALID_ARGUMENT` | 显式 NULL 写进 NOT NULL 列 |
| `table_data.not_null_without_default` | `INVALID_ARGUMENT` | 省略 NOT NULL 列且无默认值可兜底 |
| `table_data.column_not_insertable` | `INVALID_ARGUMENT` | 列由数据库生成（`GENERATED ALWAYS` 身份列、计算列） |
| `table_data.permission_denied` | `PERMISSION_DENIED` | 缺少 INSERT 权限 |
| `metadata.object_not_found` | `NOT_FOUND` | 目标对象不存在 |
| `metadata.object_kind_unsupported` | `INVALID_ARGUMENT` | 种类不适用于该操作 |
| `metadata.permission_denied` | `PERMISSION_DENIED` | 数据库拒绝读取元数据 |
| `metadata.filter_invalid` | `INVALID_ARGUMENT` | 过滤条件非法 |

## 代价

**过滤**

- 大小写敏感性随数据库（SQLite `LIKE` 对 ASCII 不敏感、MariaDB 取决于排序规则），
  同一关键词在两个连接上结果可能不同；文档必须写明差异，前端不得二次过滤掩盖；
- `LIKE '%x%'` 无法用索引，大表子串搜索会扫表（这是数据库侧代价，不是内存代价）；
- 条件进指纹意味着「改一个字就要从第一页重新搜」：翻页中途改条件是明确错误
  （`config.invalid`），不是自动重来。

**DDL**

- `exact = false` 的路径多出两项义务（说明 + 版本），拼装实现升级必须同步版本号；
- `source_detail` 是给人读的文本，跨语言/跨版本不保证措辞一致，只保证「说得清缺什么」。

**插入行**

- **一行坏、整批弃**：500 行里第 3 行唯一冲突，整批不落库。用户必须修好再整批重试；
  换来的是「要么全成要么全不成」的可预期结果（前端提示必须写清，不能只说「部分失败」）。
  被否方案（逐行保留）与 `ApplyRowChanges` 的逐行冲突保留更像，但那是「每行独立定位」的场景，
  插入没有这个语义。
- 不可取消（见上）；
- 需要完整主键回传，`Composite PK + 服务端默认值` 的组合可能拿不到生成值：这种驱动不能声明
  支持插入，宁缺毋滥。

**兼容性**

- `ListNodesRequest.filter`（3）、`GetDdlResponse.source/source_detail/generator_version`（3–5）
  都是追加字段：老服务端忽略它们、老客户端不读它们；老客户端只看 `exact` 仍然正确。
- 新增 RPC `InsertRows` 对老客户端不可见；rpc-server 目前返回明确的
  `capability.unsupported`（`UNIMPLEMENTED`），不返回空结果冒充成功。

## 冻结口径的读法（记录，不改口径）

口径第 4 条写「无 `transaction_id` 时整批成功才提交，指定事务时用 savepoint；单行失败不中断
其余行」。本文按**字面**实现为「整批原子」：任一行失败 → `rolled_back = true`、净影响 0，
但继续执行其余行以报全失败。另一种读法是「逐行保留、只把失败行报出来」（更接近
`ApplyRowChanges` 的逐行冲突保留）。

两种读法在 **proto 层都可表达**（`rolled_back` 为 false 即逐行保留），差别只在驱动实现与
前端提示。本 ADR 取字面读法，并把差异记入 PR「待定」，由编排者确认；若改为逐行保留，
proto 字段一个都不用改。

## 验证

- `backend/crates/driver-api/tests/v022_contract.rs`（10 例）：默认 SPI 实现返回
  `capability.unsupported`、DDL 自证、指纹格式、批次钳制、省略 vs NULL、错误码分类；
- `backend/crates/rpc-server/tests/contract_v022.rs`（5 例）：新字段号/枚举取值、
  `InsertRows` 只能追加、9 个新错误码 → gRPC 状态 + trailing metadata 映射；
- 变红验证：把 `InsertRow.values` 的字段号改成 9 → `insert_rows_field_numbers_are_frozen`
  失败；把 `to_status` 里 `PermissionDenied` 分支删掉 → `v022_error_codes_map_to_grpc_status`
  失败；把 `insert_rows` 默认实现改成 `Ok(InsertReport::default())` →
  `default_insert_rows_reports_unsupported_instead_of_empty_success` 失败。
  （实际执行过程见 PR 正文。）

## 重新评估条件

- 需要跨库一致的大小写搜索语义：评估统一 collation 或客户端侧归一化，不能靠驱动各自为政；
- 子串搜索在大表上不可接受：引入 FTS/倒排索引，届时单独立 ADR（涉及索引失效与写入放大）；
- 插入批次需要取消：追加 `query_id` 并复用 `CancelRowChanges`（不改既有字段号）；
- 插入需要「部分成功保留」：需改前端提示与错误清单语义，单独立 ADR，不能只翻 `rolled_back`；
- 出现 PostgreSQL（M5）：`GENERATED ALWAYS` 身份列与 `RETURNING` 会让插入路径的键回传多一种
  实现，契约字段不变。
