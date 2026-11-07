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
   `EXACT`）；选择精确计数时它是一次独立查询，必须可取消、有超时，且与本次取页在同一
   会话执行锁内（ADR 0008）。
3. **排序与筛选必须下推**：SQL 由各驱动方言拼装，列名必须用 `GetColumns` 的白名单校验，
   值一律参数化绑定——**禁止**把用户输入拼进 SQL 文本。未知列 / 不支持算子 / 类型不兼容
   在发送前返回 `config.invalid` 或 `capability.unsupported`，不让数据库报语法错。
4. **offset 分页不保证跨页稳定**：翻页期间有插入或删除会出现重复或漏行。这是 offset 的
   固有属性，必须在 proto 注释、文档与 UI 三处说明；UI 在 `Last Refresh` 旁边提供刷新入口。
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
