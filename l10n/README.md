# `l10n/` 文案目录 schema（ADR 0031：界面国际化）

这里三端共享的界面文案**唯一真值**。运行时不解析这里的文件：构建期由
`tools/l10n_gen.py` 生成 Linux 的 Rust catalog（`frontend/linux/build.rs` 调
`rust` 子命令写进 `OUT_DIR/l10n_catalog.rs`）与 Windows 的
`Strings.{en,zh}.resw` + `Strings.g.cs`（`resw` 子命令，S6/#204 接线）。

## 文件布局

- 一个功能区一个文件：`shell.toml`、`tabs.toml`、`status_bar.toml`、`startup.toml`…
  区名直接用 UI 注册表（`docs/ui-specification/components/`）的区，不发明第二套地名。
- 本表当前只落 `shell.toml`（S1/#199）；其余区随各区迁移切片（#200–#204）追加。

## 条目格式

```toml
[[entry]]
key = "shell.menu.view_language"   # 「区.蛇形名」，段名全小写，首段=区名
zh = "语言"                         # 必填，且必须与代码内原字面量逐字节相等
en = "Language"                    # 可缺（缺 = 告警，见「门禁」）
status = "done"                    # done | pending_en（待译，S7 清零对象）
```

可选复数字段（ADR 0031 决策 5，只做 one/other 两态）：

```toml
[[entry]]
key = "tabs.status_row_count"
zh = "{n} 行"        # 中文恒用这条：zh 不随 n 变化
en = "{n} rows"      # other 形态
en_one = "1 row"     # 英文 n==1 形态；缺 en_one = 该 key 无复数
status = "done"
```

- **zh 是 source of truth**：注册表与行为契约继续记中文原文；翻译只活在这里。
- key 名规则：`^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$`，生成器强制。
- 字段只允许 `key/zh/en/status/en_one/zh_one`，出现别的字段名直接报错（防拼错后
  静默丢翻译）。
- 插值只支持 `{name}` 命名占位符（Linux `l10n::tf` / Windows `Strings.T` 侧原样
  替换），不做位置参数。

## 运行时语义

- 查找与回落（Linux `src/l10n.rs`，Windows 生成的 `Strings.T` 同口径）：
  当前语言 → `en` → `zh` → key 本身，**永不空白**。
- 复数 API `t_n(key, n)` / `Strings.Tn(key, n)`：中文恒用 `zh`；英文 `n==1` 用
  `en_one`（没有则回落 `en`），否则 `en`。
- 日志行（`eprintln!` 等）**不进 catalog**，保持中文：日志是开发者界面。
- 生效方式：切换语言**重启生效**（决策 6），菜单见「视图 → 语言」。

## 门禁（生成器行为）

| 情况 | 结果 |
|---|---|
| 重复 key | 报错，退出码 1（= 构建失败，决策 3） |
| 缺 `zh` | 报错，退出码 1 |
| key 名 / 字段名不合法 | 报错，退出码 1 |
| 缺 `en` | 告警（stderr 逐行 `[l10n] en 缺失：<key>`），不阻断；发版前必须清零（S7/#205 接 release-check） |
| `zh_one` 无对应 `en_one` | 告警，按「无复数」处理 |

自检：`python3 -m unittest discover -s tools`。

## 术语

译法以 `terms.md` 为准：同一中文在不同区必须译成同一个英文词。新术语先补
`terms.md` 再进 `*.toml`。
