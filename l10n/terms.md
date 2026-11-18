# 术语表（ADR 0031 决策 4）

冻结「同一中文 → 同一英文」，防止同一个词在不同功能区译出两个样。
本表随 S1 起步，只收**外壳级种子**；各区迁移切片（#200–#204）遇到新术语先补这里，
再写进 `l10n/*.toml`。

## 规则

1. 表内译法是硬约束：`*.toml` 里的 `en` 用到这些词时必须与表一致。
2. 界面语言名用**自译名**（语言名永远用该语言书写）：`简体中文` / `English`
   在两种界面语言下都原样显示，不译成 "Chinese"。
3. 已有英文原文（如 `Query`、`Export` 这类本就是英文的入口词）不重复翻译，
   保持逐字相同。
4. 动词性条目（按钮）用命令式、首字母大写、无句号；标题句首大写；正文是完整
   句子，带句号。

## 种子表

| 中文 | English | 备注 |
|---|---|---|
| 提交 | Commit | 数据变更提交（submit 语境同词） |
| 事务 | Transaction | |
| 回滚 | Rollback | 按钮用动词 Roll Back 时按条目文本，术语本身 Rollback |
| 刷新 | Refresh | |
| 连接 | Connect | 动词（建立会话）；名词（一条连接）用 Connection |
| 查询 | Query | |
| 导出 | Export | |
| 语言 | Language | |
| 跟随系统 | Follow System | 主题与语言菜单同译 |
| 重启 | Restart | |
| 稍后 | Later | 对话框延后按钮 |
| 取消 | Cancel | |
| 深色模式 | Dark Mode | |
| 界面密度 | Density | 菜单小节名，Windows 短标签口径 |
| 文件 | File | 主菜单顶层 |
| 视图 | View | 主菜单顶层 |
| 帮助 | Help | 主菜单顶层 |
| 主菜单 | Main Menu | headerbar 菜单按钮 tooltip |
| 退出 | Quit | 菜单条目（应用级） |
| 新建 | New | 「新建连接 / 新建查询」复合词 |
| 新建查询标签 | New Query Tab | 菜单与工具条同词 |
| 关于 | About | 「关于 Squarvo」；品牌名不译 |
| 浅色 | Light | 深色模式条目 |
| 深色 | Dark | 深色模式条目 |
| 库 | Database | 客户端的 database 层；Schema 层原样写 Schema |
| 未指定 | Unspecified | 会话 / 库真值缺失的占位 |
| 未连接 | Not connected | 状态栏与树连接态 |
| 已断开 | Disconnected | 会话失效态 |
| 重试 | Retry | 错误页 / 对话框按钮 |
| 重新连接 | Reconnect | 断开错误页按钮（动词） |
| 默认 | Default | 表标签标题的空 catalog 占位 |
| 标签 | Tab | 标签条语境 |
| 会话 | Session | 界面语境（区别于终端 shell） |
| 脏行 | Dirty Row | 「提交所有脏行的变更」等 |
| 首页 | First | 分页器按钮 |
| 上一页 | Previous | 分页器按钮 |
| 下一页 | Next | 分页器按钮 |
| 末页 | Last | 分页器按钮 |

## S2（对话框与连接，#200）新增

| 中文 | English | 备注 |
|---|---|---|
| 新建连接 | New Connection | 与 `shell.menu.new_connection` 同译 |
| 编辑连接 | Edit Connection | |
| 保存 | Save | 按钮 |
| 保存并连接 | Save & Connect | |
| 测试连接 | Test Connection | |
| 驱动 | Driver | |
| 别名 | Alias | |
| 主机 | Host | |
| 端口 | Port | |
| 用户名 | Username | |
| 密码 | Password | |
| 凭据 | Credential | 「凭据引用」credential reference |
| 系统凭据库 | System Credential Store | |
| 只读 | Read-Only | 「只读打开」Open Read-Only |
| 浏览 | Browse | |
| 数据库文件 | Database File | |
| 连接参数 | Connection Parameters | |
| 未选择驱动 | No Driver Selected | |
| 知道了 | Got It | 提示对话框按钮 |
| 取消 | Cancel | 沿用种子表 |
| 放弃更改并关闭 | Discard Changes and Close | |
| 放弃更改并退出 | Discard Changes and Quit | |
| 保存并关闭 | Save and Close | |
| 回滚事务并关闭 | Roll Back Transaction and Close | 按钮用动词短语 |
| 关闭标签 | Close Tab | 「关闭标签？」Close Tab? |
| 无法新建标签 | Cannot Open New Tab | 超限提示标题 |
| 未提交的更改 | Uncommitted Changes | 关窗守卫标题同词 |
| 握手 | Handshake | 「正在与数据库握手…」 |
| 关于 | About | 沿用种子表；「关于 Squarvo」About Squarvo |
| 后端 | Backend | 「后端诊断」Backend Diagnostics |
| 端点文件 | Endpoint File | |
| 传输方式 | Transport | |
| 协议 | Protocol | |
| 版本 | Version | |
| 是 / 否 | yes / no | 句中布尔值，小写 |
| 日志 | Log | 「Sqarvo 日志」Sqarvo Log |
| 复制路径 | Copy Path | |
| 已复制 | Copied | tooltip |
| 配置目录 | Config Directory | |
| 配置文件 | Config File | |
| 密钥条目 | Key Entry | 系统凭据库中的条目 |
| 标签上限 | Tab Limit | 超限提示正文 |

## S3（对象树与浏览，#201）新增

| 中文 | English | 备注 |
|---|---|---|
| 表 | Table | 对象种类；`Tables` 分组条目同词 |
| 视图 | View | 对象种类；与主菜单「视图」同词（靠条目名区分语境） |
| 函数 | Function | 对象种类 |
| 存储过程 | Procedure | 对象种类 |
| 序列 | Sequence | 对象种类 |
| 索引 | Index | 对象种类；复数 Indexes |
| 触发器 | Trigger | 对象种类 |
| 字段 | Field | 表结构分组里的列条目 |
| 列 | Column | 列清单 / 网格语境 |
| 库 | Database | 沿用种子表；Schema 层写 Schema |
| 模式 | Schema | 库层之下的命名空间 |
| 外键 | Foreign Key | 表下的二级分组 |
| 分组 | Group | 树上的分组行 |
| 浏览会话 | Browse Session | 对象树 / 搜索 / DDL 用的那条会话 |
| 连接注册表 | Connection Registry | `connections.json` 里的连接清单 |
| 占位行 | Placeholder Row | 读取中 / 加载更多 / 失败 / 空 / 提示行 |
| 转储表 | Dump Table | 右键条目（建表语句 + INSERT） |
| 断开 | Disconnect | 沿用「连接 Connect」；名词态写 disconnection |
| 刷新这一组 | Refresh This Group | 右键条目 |
| 筛选 | Filter | 条件列表与面板 |
| 排序 | Sort | 表头与面板 |
| 分页 | Pagination | 分页器与页码口径 |
| 总数 | Total | 精确 / 估计 / 未统计三态 |
| 行号区间 | Row Range | 「第 501–1000 行」 |

## S3 Lane B（信息面板、Objects 页与恢复对话框，#201）新增

| 中文 | English | 备注 |
|---|---|---|
| 信息面板 | Info Panel | 面板标题与空态头部同词 |
| 表信息 | Table Info | `[DDL] / [表信息]` 切换条 |
| 地址 | Address | 连接视图字段 |
| 用户 | User | 连接视图字段 |
| 默认库 | Default Database | 连接视图字段；对话框的「默认库」输入同词 |
| 服务端版本 | Server Version | 连接视图字段 |
| 连接延迟 | Connection Latency | 连接视图字段 |
| 会话数 | Session Count | 连接视图字段 |
| 计数说明 | Count Basis | 库视图「已展开结果」说明行 |
| 名称 | Name | 库视图字段与 Objects 第二列表头（Linux 原文「表名」） |
| 类型 | Type | Objects 第三列表头 |
| 表名 | Name | Objects 表头按列内容译；与「名称」同译 |
| 行号 | Row | Objects / 数据网格第一列表头 |
| 双击打开 | Double-click to open | Objects 行 tooltip 后缀 |
| 主键 | Primary Key | 表信息分节 |
| 主键列 | Primary Key Columns | 表信息行标签 |
| 无主键 | No Primary Key | 主键空态值 |
| 无索引 | No Indexes | 沿用「索引 Indexes」复数 |
| 无触发器 | No Triggers | 沿用「触发器 Triggers」 |
| 无默认值 | No Default | 列默认值四态之一 |
| 已加载行数 | Loaded Rows | 表信息分节 |
| 原始类型 | Raw Type | 列信息字段 |
| 可空 | Nullable | 列信息字段与值同词 |
| 默认值 | Default Value | 列信息字段 |
| 字符集 | Charset | 列信息字段 |
| 排序规则 | Collation | 列信息字段 |
| 不适用 | Not Applicable | 字段缺失的稳定占位（协议口径） |
| 读取失败 | Read Failed | 状态行 / 分节标题；句中做谓语时小写 read failed |
| 正在读取 | Reading | 状态行进行中 |
| 还有更多 | More | 状态行后缀 `（还有更多）` → "(more)" |
| 尚未实现 | Not Implemented | 「函数列表尚未实现」等如实说明 |
| 查看日志 | View Log | 恢复对话框按钮；「日志 Log」沿用种子表 |
| 需要重启 | Needs to Restart | 「Sqarvo 需要重启」标题；品牌名不译 |

## S4 Lane A（数据网格，#202）新增

| 中文 | English | 备注 |
|---|---|---|
| 列显隐 | Column Visibility | 工具条按钮与浮层标题同词 |
| 筛选与排序 | Filter & Sort | 工具条按钮与面板标题同词；「筛选 Filter」「排序 Sort」沿用 S3 |
| 算子 | Operator | 筛选条件三件套之一 |
| 值 | Value | 筛选值输入与「字段 / 值」列头同词 |
| 排序键 | Sort Key | 面板条目与消息 |
| 筛选条件 | Filter Conditions | 面板分节标题 |
| 列清单 | Column List | 「来自 GetColumns 的列清单」等 |
| 应用 | Apply | 面板按钮（提交面的「提交 Commit」不在此词） |
| 清空 | Clear | 面板按钮 |
| 已应用 | Applied | 面板状态标签 |
| 空表 | Empty Table | 状态行与空态标题同词 |
| 无权限 | No Permission | 徽标 |
| 未统计 | Not Counted | 徽标；状态口径「未统计（超出计数预算）」见 `object_tree.total_skipped` |
| 服务端 | Server | 「服务端标记为只读」等；后端进程仍译 Backend |
| 剪贴板 | Clipboard | |
| 驱动不支持排序/筛选 | Sort/Filter Unsupported | 徽标短标签；句中写 "does not support sorting" |

## S4 Lane B（编辑与提交面，#202）新增

| 中文 | English | 备注 |
|---|---|---|
| 设为 NULL | Set to NULL | 「SQL 空值」= (SQL null) |
| 显式 NULL | Explicit NULL | 新增行编辑器（与「省略」相对） |
| 省略该列 | Omit this column | 新增行草稿的第三态 |
| 省略 | Omit | 「该列省略：…」提示与「省略 → NULL」 |
| 数据库生成 | generated by the database | 「（数据库生成）」单元格占位文本 |
| 标记删除 | mark for deletion | 「已标记删除 N 行」「N 行标记删除」 |
| 撤销删除 | undo the deletion | 与「放弃更改 Discard Changes」区分 |
| 自动提交 | Auto-Commit | 事务徽标；句中说明写 auto-commit |
| 事务中 | In Transaction | 徽标；「事务中（只读）」In Transaction (Read-Only) |
| 开始事务 | Begin Transaction | 按钮；状态句首大写 Begin transaction |
| 提交事务 | Commit Transaction | 按钮；「正在提交事务…」Committing the transaction… |
| 回滚事务 | Roll Back Transaction | 按钮（动词短语，沿用 S2「回滚事务并关闭」口径） |
| 取消提交 | Cancel Commit | 在途时提交按钮的文案 |
| 正在取消… | Cancelling… | 取消在途按钮文案 |
| 待提交 | to commit | 脏行计数标签「脏行 N · 待提交 M 条」 |
| 隔离级别 | isolation level | 卡片与说明 |
| 读未提交 | Read Uncommitted | 级别名（括注 SQL 原文 READ UNCOMMITTED） |
| 读已提交 | Read Committed | 同上 |
| 可重复读 | Repeatable Read | 同上 |
| 可序列化 | Serializable | 同上 |
| 快照 | Snapshot | 同上（本 UI 不提供该级别） |
| 默认（由数据库决定） | Default (decided by the database) | 隔离级别「未指定」档 |
| 脏读 | dirty read | 「脏读可见：…」 |
| 共享锁 | shared lock | 「读也加共享锁…」 |
| 风险确认 | risk confirmation | 阈值与确认对话框 |
| 确认提交 | Confirm Commit | 风险对话框按钮 |
| 断连清理 | disconnect cleanup | 「数据库未变化」等批次取消口径 |
| 行边界 | row boundary | 「可在行边界中止」 |
| 整批回滚 | rolled back as a whole batch | InsertRows 与应用批次同一词 |
| 逐行失败 | per-row failure | 「失败行摘要」条目 |
| 值转换 | value conversion | 「不是合法的 … 值」族 |
| 有限浮点数 | finite float | 「{text} 不是有限浮点数」 |
| 小数秒 | fractional second | 时间解析错误族 |
| 时区偏移 | UTC offset | 时间戳解析错误族 |
| 复制为 | Copy as | 行右键菜单二级标题；「复制为 INSERT」Copy as INSERT |
| 删除行 | Delete Row | 行右键菜单条目 |

## S4 Lane C（DDL 状态页与元数据错误副文本，#202）新增

| 中文 | English | 备注 |
|---|---|---|
| 元数据 | Metadata | `MetadataError` 族标题与副文本 |
| 没有读取元数据的权限 | No Permission to Read Metadata | 状态页标题；「无权限 No Permission」徽标短标签另立 |
| 对象种类 | Object Kind | 「该对象种类不支持这个操作」；proto `ObjectKind` 的界面词 |
| 平铺表列表 | Flat Table List | Unsupported hint 里指向的对象树视图 |
| 翻页状态 | Pagination State | 「翻页状态已失效」；「分页 Pagination」同族 |
| 控制字符 | Control Character | FilterInvalid hint |
| 客户端拼装 | Synthesized by the client | DDL 警示行；`exact = false` 的界面说法 |
| 数据库原文 | The database's original text | DDL 警示行 |
| 信息有损失 | Information is lossy | DDL 警示行 |
| 行内编辑 | Inline editing | `type_not_editable_reason`；网格语境与「编辑 Edit」区分 |
| 未映射类型 | Unmapped type | 「二进制 / 数组 / 未映射类型」族 |
| 会话已失效 | Session Is No Longer Valid | SessionLost 标题（数据库会话） |
| 本地后端不可用 | Local Backend Unavailable | Unavailable 标题；「后端 Backend」沿用 S2 |
