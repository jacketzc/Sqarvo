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
