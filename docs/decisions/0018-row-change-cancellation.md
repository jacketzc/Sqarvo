# ADR 0018：行变更批次的取消语义

- 状态：已接受
- 日期：2026-11-05
- 关联：Issue #70、#61、#69；ADR 0005（查询游标与取消）、ADR 0008（会话并发）、ADR 0017（行编辑）

## 背景

`ApplyRowChanges` 一批最多 500 行，每行一条参数化 DML，在慢库上整批要几秒到几十秒，
而整批期间**一直持着**会话执行锁（ADR 0008）。此前没有任何取消通道：用户只能等，
或者断开连接（那会把事务一起丢掉）。

`QueryService.Execute` 早就有 `query_id` + `Cancel` 的完整语义（ADR 0005）。缺口是：

1. 行变更批次没有可被取消的标识；
2. 驱动没有「在批次中间观察取消」的位置；
3. 取消路径若走会话（`acquire` + 执行锁），会排在批次后面，等于没取消——在批次内部
   等待同一把锁还可能直接死锁。

## 备选方案

1. **复用 `QueryService.Cancel`**，把行变更批次的 `query_id` 登记进同一张查询登记表。
   - 优点：改动面最小，ID 生成、并发上限、断连清理都现成。
   - 缺点：语义混业——`QueryService.Cancel` 的含义是「中断查询并释放结果游标」，
     而写入批次的取消要「在行边界停下并回滚本批」；两者的返回体、失败模式、生命周期都
     不同。`QueryRegistry` 的 `QueryState`（Running/Cancelling/Cancelled）也没有「已回滚」
     这一状态，硬塞进去会让 `query.cancelled` 同时表示两种完全不同的结果。
2. **新增 `TableDataService.CancelRowChanges`**（本决策）。
3. **用驱动原生取消接口打断语句**（SQLite `InterruptHandle`、MariaDB `KILL QUERY`）。
   - 优点：能取消单条极慢的 DML。
   - 缺点：语句被打断后 SQLite **隐式回滚整个事务**、MariaDB 的事务是否存活取决于服务端，
     与「事务模式下列已取消、外层事务仍可用」的契约直接冲突；而且取消会落在语句中间，
     做不到「行边界」语义。否决。

## 决策

采用方案 2：`TableDataService.CancelRowChanges` + 客户端提供 `query_id` + 驱动行间观察的
取消令牌。

### 契约

- `ApplyRowChangesRequest` 追加 `string query_id`（字段 7）。**由客户端生成**：一元 RPC
  在返回前无法把服务端生成的 ID 告诉调用方，只有客户端提供的 ID 才可能在执行期间被取消。
  留空时服务端自己生成并在响应里回显，该批次因此不可取消（proto 注释已写明）。
- `ApplyRowChangesResponse` 追加 `cancelled`、`executed_changes`、`query_id`（字段 5/6/7）。
  取消是**正常结果**而不是错误：`cancelled = true` 时 `results` 为空、
  `total_rows_affected = 0`（已执行的行都回滚了），`executed_changes` 是取消前真正执行过的
  条数——这是「真的跑到一半」的唯一可观测证据。
- `CancelRowChanges(CancelRowChangesRequest{query_id}) -> Empty`。幂等：重复取消、取消不存在
  或已结束的批次都返回成功；驱动不支持取消时返回 `capability.unsupported`，绝不假装成功。

### 取消怎么生效（不死锁的前提）

- 取消方只做一次 `AtomicBool` 置位（`RowChangeCancel`）：**不 acquire 会话、不取会话执行锁、
  全程无 `.await`**。服务端实现里 `cancel_row_changes` 连 `SessionManager` 都不碰。
- 令牌在执行**之前**就登记进 `RowChangeBatches`（`sqarvo-connection`），所以取消请求不会落进
  「还没登记」的窗口；批次结束时由 RAII 守卫移除登记。
- 驱动在**每条变更之前**读一次令牌：命中后不再执行后续变更，跳出循环，由 `apply` 走回滚分支。
  已开始的那条 DML 一定执行完——取消只发生在行边界，语句不会被撕成两半。
- 自动提交模式（无 `transaction_id`）：驱动自开的整批事务 `ROLLBACK`，库里不留半批。
- 事务模式：`ROLLBACK TO SAVEPOINT` + `RELEASE`，只回滚本批，外层事务继续可用
  （可写、可提交、可回滚）。

### 锁顺序

不变：执行锁 → 事务登记锁。取消路径不取任何一把，因此不存在「取消等执行锁、执行锁等取消」
的环。ADR 0008 的表格里新增一行「取消行变更批次 / `CancelRowChanges` / 否」。

## 取舍

- 多一个 RPC、两组 proto 字段、一张进程内登记表（上限 64 批，断连时清理并请求取消）。
- 客户端必须自己生成 `query_id`；旧客户端留空仍可提交，只是无法取消。
- 取消粒度是行边界：单条 DML 极慢（超大 BLOB、长事务锁等待）时，取消要等这一行结束。
  这是「事务仍可用」的代价，也是被接受的理由。
- 取消不提前释放会话执行锁与连接：批次停下前仍占着锁，其他请求继续排队（ADR 0008 的
  单会话串行语义不变）。
- `executed_changes` 是「已执行并随后回滚」的条数，不是「已生效」条数；界面必须显示
  「已取消（回滚 N 行）」，不能显示成「已修改 N 行」。

## 重新评估条件

- 需要亚行级取消（单行就耗秒级）：只能回到驱动级中断，并重新设计中断后的事务语义；
- 需要进度推送而不是结束时报数：把令牌扩展成 `watch` 通道（每 N 行发一次进度），
  登记表结构不变；
- 出现 `KILL QUERY` 后事务仍可用的数据库，可在该驱动内改用原生取消，同时保留行边界检查
  作为兜底（两条路都必须整批回滚）。
