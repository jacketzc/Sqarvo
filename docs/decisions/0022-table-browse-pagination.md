# ADR 0022：表数据浏览的分页、排序、筛选与总数

- 状态：已接受（2026-11-07 评审通过）
- 日期：2026-11-07
- 关联：Issue 待建；REQ-META-005（改写）、REQ-UI-001；
  ADR 0005（游标所有权）、ADR 0017（表数据与行编辑）、ADR 0021（标签与会话）
- 上游：[workspace-model.md](../ui-specification/workspace-model.md) §6

## 背景

`TableDataService.BrowseTable` 现在是「固定主键升序 + 不透明 cursor」，`REQ-META-005` 明确
写着**禁止 `OFFSET`**。理由是当时只有「滚动到底自动加载」这一种交互，游标能保证翻页期间
不重复不遗漏。

v0.2.3 的表格要按对象平台的习惯提供：页码分页器、`Record N of M in page P`、列头排序、
筛选与列显隐。这些都需要在契约里表达「从第 N 行开始」「按什么排序」「满足什么条件」
「总共有多少行」，而现有请求只有 `page_size` + `page_token`。

因此必须先决定分页方式——它会固化到三个前端、两套（未来三套）驱动与 proto 里。

## 备选方案

1. **保持纯 cursor**：契约零改动，但页码、跳页、总数都做不出来；表格工具条只能落一半。
2. **纯 offset**：页码与总数直白，实现简单；代价是翻页期间数据变化会出现重复/漏行，
   且大 offset 需要扫描前面所有行。
3. **双模**：连续翻页用 cursor（稳定），页码跳转用 offset（有代价，可接受）。
   契约要同时承载两种方式，调用方要理解两者差别；但可以做到「老客户端行为完全不变」。

## 决策

采用方案 3 的双模，但**以 offset 为主、cursor 保留兼容**。

### 请求（向后兼容，只做增量）

```proto
message BrowseTableRequest {
  // ... 现有 object = 1, page_size = 2, page_token = 3 不变
  // 起始行偏移；与 page_token 互斥，两者都非空 → config.invalid。
  uint64 offset = 4;
  // 排序键；空 = 主键升序（v0.2.2 行为）。
  repeated SortKey sort = 5;
  // 结构化筛选；未设置 = 不过滤。v0.2.3 只支持 AND 组合。
  Filter filter = 6;
  // 是否请求总行数（代价见下）。false 时 total_kind 为 UNKNOWN。
  bool include_total = 7;
}

message SortKey {
  string column = 1;                  // 必须来自 GetColumns 的列名，服务端校验
  SortDirection direction = 2;        // SORT_DIRECTION_ASC / DESC
}

enum SortDirection { SORT_DIRECTION_UNSPECIFIED = 0; SORT_DIRECTION_ASC = 1; SORT_DIRECTION_DESC = 2; }

message Filter {
  repeated Condition conditions = 1;  // 全部 AND
  message Condition {
    string column = 1;
    FilterOperator operator = 2;
    // 值的表达式，复用 ColumnValue 的 kind；IS NULL / IS NOT NULL 时忽略。
    sqarvo.common.v1.ColumnValue value = 3;
  }
}

enum FilterOperator {
  FILTER_OPERATOR_UNSPECIFIED = 0;
  FILTER_OPERATOR_EQ = 1;
  FILTER_OPERATOR_NE = 2;
  FILTER_OPERATOR_LT = 3;
  FILTER_OPERATOR_LE = 4;
  FILTER_OPERATOR_GT = 5;
  FILTER_OPERATOR_GE = 6;
  FILTER_OPERATOR_LIKE = 7;
  FILTER_OPERATOR_IS_NULL = 8;
  FILTER_OPERATOR_IS_NOT_NULL = 9;
}
```

### 响应

```proto
message BrowseTableResponse {
  // ... 现有字段 1-7 不变
  uint64 total_rows = 8;              // 仅在 total_kind != UNKNOWN 时有意义
  TotalKind total_kind = 9;           // UNKNOWN / ESTIMATE / EXACT
}

enum TotalKind { TOTAL_KIND_UNKNOWN = 0; TOTAL_KIND_ESTIMATE = 1; TOTAL_KIND_EXACT = 2; }

enum RowOrder {
  // ... 现有值不变
  ROW_ORDER_REQUESTED_SORT = 3;       // 按请求的排序；页间稳定性取决于排序列唯一性
}
```

### 语义约束

1. **总数三态必须显式**，客户端不得推断：
   - `EXACT`：服务端真实计数（SQLite `count(*)`、MariaDB 独立 `SELECT COUNT(*)`）；
   - `ESTIMATE`：来自统计信息（MariaDB `information_schema.TABLES.TABLE_ROWS`，InnoDB
     误差可达数十个百分点）；
   - `UNKNOWN`：未请求，或请求了但驱动无法给出。
   UI 文案随之区分「5 行」与「约 5 行」；`record N of M` 在 ESTIMATE 下改写成
   「第 P 页 · 共约 M 行」。
2. **精确计数的成本**：`include_total=true` 时，驱动可以只返回 `ESTIMATE`（不得伪称
   `EXACT`）；选择精确计数时它是一次独立查询，必须**有界**（`timeout_millis`，默认 2 s、
   上限 60 s），且与本次取页在同一会话执行锁内（ADR 0008）。到点由驱动用自身的服务端中断
   机制结束这条计数语句，返回 `UNKNOWN` + `total_skipped = true`，**不得**报错、不得返回
   半截数量、不得影响本页数据。**客户端取消不在本版**：`BrowseTable` 是一元 RPC，响应返回前
   没有取消通道（理由与候选方案见文末「修订记录」）。
3. **排序与筛选必须下推**：SQL 由各驱动方言拼装，列名必须用 `GetColumns` 的白名单校验，
   值一律参数化绑定——**禁止**把用户输入拼进 SQL 文本。未知列 / 不支持算子 / 类型不兼容
   在发送前返回 `config.invalid` 或 `capability.unsupported`，不让数据库报语法错。
4. **offset 分页不保证跨页稳定**：翻页期间有插入或删除会出现重复或漏行。这是 offset 的
   固有属性，必须在 proto 注释、文档与 UI 三处说明；刷新入口在状态栏行操作组（v0.2.3.3
   起 `Last Refresh` 旁边的重复按钮已删除，同一动作只留一个入口）。
   连续翻页的稳定性只在「不带 sort、用 page_token」时成立（v0.2.2 行为）。
5. **大 offset 的代价**：MariaDB `LIMIT 100000,100` 需扫描前面 10 万行。v0.2.3 的界面
   只提供首页 / 上页 / 下页 / 末页，页码只读显示；跳转到任意页码需要额外的口径（例如
   按主键估算位置），不在本版提供。
6. **无主键表与视图**：允许排序、筛选与计数，`read_only` 仍为 true（编辑定位依赖主键，
   ADR 0017 不变）。排序不改变只读性。
7. **越界**：`offset` 超过总数时返回空行 + `has_more=false` + 如实的总数，不返回错误。
8. **兼容**：老客户端不填新字段 → 请求落到 `offset=0`、无排序、无筛选、`include_total=false`，
   行为与 v0.2.2 完全一致；cursor 路径继续可用，不作废。

## 取舍

- 两套分页方式并存，调用方要理解差别；用「同时给 offset 与 token 就报错」强制显式选择。
- offset 的重复/漏行无法消除，只能告知；强一致需要快照事务，成本远高于本版需求。
- 精确总数在大表上可能很慢，因此做成「可选、可取消、可降级为估计」。
- 排序改变行序后，行编辑仍靠主键定位，两者互不影响；但用户看到的「第 3 行」在刷新后
  可能换位置，需要文案提示。
- 筛选只支持 AND：够覆盖「列 = 值」「列 包含 文本」的常见用法，OR 与嵌套条件本版不做。

## 重新评估条件

- 出现「必须支持跳转到任意页码」的真实需求（需要先有按主键定位的方案）；
- PostgreSQL（M5）驱动引入时，`count(*)` 成本与估计精度差异明显，需要按驱动配置策略；
- 用户数据规模让 `EXACT` 计数常态超过 2 秒，需要改成异步计数任务；
- 需要 OR / 嵌套筛选，或需要把筛选条件保存为「视图」。

## 修订记录（#127，2026-11-07 采纳 A）

### 事实

§2 写的是「精确计数…必须**可取消**、有超时」。实现 #127 时核实：

- `BrowseTable` 是**一元 RPC**：客户端在响应返回前无法表达取消，服务端也没有可用于
  匹配的请求 ID；
- 服务端唯一的取消通道 `QueryService.Cancel` 只认识 `QueryService.Execute` 登记进
  `QueryRegistry` 的 `query_id`；`BrowseTableRequest` 既没有 `query_id` 字段，服务端
  也不为浏览登记任何东西；
- 仓库里没有为浏览提供任何服务端可观测的取消输入：客户端断开只表现为传输层事件，
  handler 内拿不到「用户取消」这个判断；这一点与 `CancelRowChanges` 的设计动机同源
  （一元 RPC 必须在发出请求前就准备好取消通道，见 ADR 0018）。

结论：在**不改契约**的前提下，「可取消」无法实现。本版实现的是 §2 的另一半，
并把它做实：`timeout_millis`（默认 2 s、上限 60 s）+ 驱动侧服务端中断
（SQLite `sqlite3_interrupt` / MariaDB `KILL QUERY`），超时返回
`TOTAL_KIND_UNKNOWN` + `total_skipped = true`，不报错、不影响本页。

### 方案 A（已采纳，现行行为）

把 §2 改为：

> 选择精确计数时它是一次独立查询，**必须有界**（`timeout_millis`，默认 2 s、上限 60 s），
> 且与本次取页在同一会话执行锁内（ADR 0008）。到点由驱动用自身的服务端中断机制结束
> 这条计数语句，返回 `UNKNOWN` + `total_skipped = true`，**不得**报错、不得返回半截
> 数量、不得影响本页数据。客户端取消不在本版：一元 RPC 在响应返回前没有取消通道。

代价：用户不能主动结束一次仍在预算内的计数（最长等 60 s，默认 2 s）。

### 方案 B（未采纳，保留为未来候选）

给 `BrowseTableRequest` 追加 `query_id`（客户端生成，格式与 `ExecuteQueryRequest`
一致），服务端登记到一个与 `RowChangeBatches` 同形的取消表，并让 `QueryService.Cancel`
（或新的 `CancelTableBrowse`）只置位令牌、不取会话执行锁；驱动在计数的超时看门线程里
同时检查「超时」与「取消」两个条件。收益是真正的用户取消，代价是：

- 再增加一个 proto 字段与一张全局登记表（含断连清理）；
- 取消与超时共用同一条中断路径，语义要在 proto 上写清「取消 → 也是 skipped，不报错」；
- 一元模型下客户端仍只能**预先**提供 ID，不能在中途「发现慢再取消」——UI 必须在
  发请求前就准备好 ID。

**采纳结论（2026-11-07）**：A 已生效，正文 §2 按 A 的措辞执行（代码在 #127 / PR #130 落地：
`timeout_millis` 默认 2 s、上限 60 s，超时返回 `UNKNOWN` + `total_skipped`）。B 未采纳，
保留为未来候选：等「用户数据规模让 EXACT 计数常态超过 2 秒」（见「重新评估条件」）成真、
或前端真的需要「停止统计」按钮时再落；B 落地后本节的 A 改为历史说明。

## v0.2.3.2：整表导出复用 offset 分页（2026-11-08，v0.2.3.2 / #154）

### 事实

v0.2.3 的「导出」只导**已加载**的行：查询结果导内存里的行、表数据导当前页。v0.2.3.2 让表数据
的 `EX` 多一个「整张表」范围，而契约里**没有任何**可用于全表导出的 RPC：没有服务端流式导出，
没有导出任务句柄，`BrowseTable` 仍是「一页一响应」。因此整表导出只能由客户端分页拼出来：

- 页大小直接复用表浏览的 `TABLE_PAGE_SIZE = 500`（`backend::table`），不新建常量；
- 每页请求沿用该标签**当前**的排序与筛选，并在导出开始时**冻结**（导出期间用户改排序不会
  把两种行序写进同一个文件），`include_total = false`——总数只是界面展示，导出逐页读到
  `has_more = false` 为止，第一页再算一次 `COUNT(*)` 只会拖慢开始时间；
- offset 步进按**实际收到的行数**而不是页大小：服务端给了短页却仍说 `has_more` 时不跳行；
  收到空页直接收尾，避免 offset 不前进的空转；
- 写盘是流式的：`CsvSink` 先写目标同目录的隐藏临时文件，逐行不整份入内存，成功后 `rename`；
  取消或失败时 `Drop` 删除临时文件，目标路径不会出现半截文件。进度**每页一条**（限流在
  产生端），界面不会被逐行事件淹没。

### 取消粒度

取消是一个 `Arc<AtomicBool>`，在**每个 RPC 之前**（页循环开头）与**每行之间**各检查一次。
因此取消粒度是：

- **行边界**：已经拿到的那一页里，取消在第 N 行与第 N+1 行之间生效，已写的行不撤回
  （临时文件整体删除）；
- **页边界**：页与页之间取消立即生效；
- **在飞的一页取消不了**：`BrowseTable` 是一元 RPC，请求发出后没有取消通道也没有客户端超时，
  必须等这一页返回才会重新检查取消位。

这与 ADR 0018（行变更取消）**同源**：一元 RPC 只能在发请求前准备好取消通道，不能在途中打断。
本版没有为此新增 `query_id` 字段或服务端登记表——那正是本 ADR「修订记录」里的方案 B，收益
（真正的服务端中断）不足以支撑为本功能单独扩契约。

### 代价

- 取消最多要等**一页（500 行）**的返回；一页在慢表/大 offset 上耗时多久，取消就要等多久。
  最坏情况是单页 RPC 挂住：此时导出线程与界面的取消按钮都在等，用户只能关标签或重启。
- 逐页 `BrowseTable` 不能保证跨页稳定（正文 §4）：导出期间有插入/删除时，文件里可能出现
  重复或漏行。这是 offset 的固有属性，本版接受，未做快照事务。
- 每页一次 RPC：大表的往返次数 = 行数 / 500，延迟敏感场景比单条流式查询多出 N 次往返。

### 重新评估条件

- 单页 500 行在目标数据上常态耗时超过数秒（此时「等一页」的取消体验不可接受）；
- 用户明确要求「导出可秒级取消」或「导出期间可继续用同一个连接」；
- 引入服务端流式导出 RPC（M8 的导入导出范围），本节的客户端分页实现改为它的降级路径；
- 出现统一的后台任务管理器（统一取消通道），届时把导出与计数一起迁到该通道上。

## 修订（v0.2.3.3）

- 背景里把「列头排序」列为 v0.2.3 要提供的能力，但表头点击路径**从未接线**（建列处没有
  手势、没有 `set_sorter`），v0.2.3.3 已按产品决定删除 `toggle_sort` / `qa_toggle_sort` /
  `SortOrder::toggle` 等死代码与 QA 的 `sort=` 步骤。**排序入口只有筛选与排序面板**
  （点列名切方向、↑↓ 调优先级、✕ 删除），表头不可点。本文档其余部分（offset / sort /
  filter / include_total、总数三态、计数有界超时）不变。
- §4 的刷新入口口径：`Last Refresh` 旁的重复刷新按钮已在 v0.2.3.3 删除，同一动作只留
  状态栏行操作组的 `⟳`（§4 正文已于此前就地注明）。行为口径以
  `docs/ui-specification/ui-behavior-contract.md` §5.3 / §5.4 为准。
