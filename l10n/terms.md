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
