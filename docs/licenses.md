# 第三方依赖许可声明

本文履行 Sqarvo 发布物对第三方依赖的署名与声明义务。Sqarvo 自身以
[Apache License 2.0](../LICENSE) 发布（`LICENSE` 是完整正文，仓库根目录），本文只处理
**依赖**：谁用了什么许可、我们需要在分发时随附哪些声明。

- 范围口径：本文覆盖**直接依赖**（`backend/Cargo.toml` 的 `[workspace.dependencies]` 与各
  crate/app 的 `Cargo.toml` 里点名的包）里许可形态不是「MIT / Apache-2.0 双许可、随附即可」
  的那几个，以及带 `NOTICE` 文件的包。传递依赖的完整清单以 `backend/Cargo.lock` 为准，
  不在本文逐条枚举——那份清单会随每次 `cargo update` 漂移，抄进文档必然过期。
- 落点理由：本仓库没有 `NOTICE` 文件先例（根目录只有 `LICENSE`，`packaging/linux/build-deb.sh`
  的 `copyright` 只声明 Sqarvo 自身的 Apache-2.0，不涉及依赖）。`docs/` 是既有惯例里唯一
  「与代码同仓、中英同目录、CI 会校验链接」的文档层，因此声明落在这里，并从
  [架构总览](architecture/overview.md) 与 [MCP 架构](architecture/mcp.md) 两处链入；
  打包脚本要在发布物里随附本文（见 §4）。

## 1. 需要署名/随附正文的依赖

### 1.1 `sqlparser-rs`（Apache-2.0，自带 NOTICE）

- 包：`sqlparser` **0.64.0**（`sqlparser_derive` 0.7.0 同许可），仓库
  <https://github.com/apache/datafusion-sqlparser-rs>，`license = "Apache-2.0"`（**单一**
  Apache-2.0，不是常见的 Apache/MIT 双许可）。
- 用途：MCP 通道的 SQL 只读策略与审计指纹（`backend/apps/sqarvo-mcp/src/policy/`），
  决策记录见 [ADR 0033](decisions/0033-sql-parser-readonly-policy.md)。
- 义务：Apache-2.0 §4 要求分发时保留版权与许可声明，且上游附带 `NOTICE` 文件时必须随附其
  内容。该包确实带 `NOTICE.TXT`，正文如下（逐字转载）：

  ```text
  Apache DataFusion sqlparser-rs
  Copyright 2024-2026 The Apache Software Foundation

  This product includes software developed at
  The Apache Software Foundation (http://www.apache.org/).
  ```

- 我们的处理：发布物（tarball、`.deb`、安装包）在包根随附 `NOTICE` 文件，内容至少包含上面
  这段原文；源码分发时本文件即等同效果。

### 1.2 `rmcp`（MCP 官方 Rust SDK，Apache-2.0，处于 MIT → Apache-2.0 过渡期）

- 包：`rmcp` **3.10.0** 与 `rmcp-macros` 3.10.0，仓库
  <https://github.com/modelcontextprotocol/rust-sdk/>，`license = "Apache-2.0"`。
- 用途：stdio 传输、`tools/*` 与 `resources/*` 协议面，见 [ADR 0032](decisions/0032-mcp-standalone-process.md)。
- 它的 `LICENSE` 文件顶部有一段许可过渡声明，属于必须原样保留的声明，摘录如下：

  ```text
  The MCP project is undergoing a licensing transition from the MIT License to the Apache
  License, Version 2.0 ("Apache-2.0"). All new code and specification contributions to the
  project are licensed under Apache-2.0. Documentation contributions (excluding
  specifications) are licensed under CC-BY-4.0.

  Contributions for which relicensing consent has been obtained are licensed under
  Apache-2.0. Contributions made by authors who originally licensed their work under the
  MIT License and who have not yet granted explicit permission to relicense remain licensed
  under the MIT License.

  No rights beyond those granted by the applicable original license are conveyed for such
  contributions.
  ```

  （上文按原样折行转载；语义上是「新代码 Apache-2.0 + 未同意重许可的历史 MIT 贡献仍是 MIT」，
  不是双许可可择一。）
- 义务：无 `NOTICE` 文件；分发时随附其 `LICENSE` 正文即可（与 §1.1 一起进 `NOTICE`/`THIRD-PARTY` 附录）。

### 1.3 其余 Apache-2.0 直接依赖（无 `NOTICE`）

按 `cargo tree -p sqarvo-mcp` 实测的许可字段，直接依赖里再没有带 `NOTICE` 文件的包：

| 包 | 版本（`Cargo.lock`） | 许可 | 用途 |
|---|---|---|---|
| `rpassword` | 7.5.5 | Apache-2.0（另附 `LICENSE-APACHE` 正文） | `sqarvo-mcp credentials set` 的终端不回显读密码 |
| `sha2` | 0.11.1 | MIT OR Apache-2.0 | 审计 SQL 指纹的 SHA-256 |
| `tokio` / `tracing` / `tracing-subscriber` | 见 lockfile | MIT | 运行时与日志（日志钉 stderr） |
| `serde` / `serde_json` / `schemars` / `clap` / `uuid` / `time` / `libc` / `directories` | 见 lockfile | MIT 或 MIT OR Apache-2.0 | 序列化、schema 派生、CLI、时间与路径 |

双许可（`MIT OR Apache-2.0`）的包我们按 Apache-2.0 分支接收，同时保留 MIT 版权声明行即可，
不需要单独 `NOTICE`。

## 2. GUI 与后端既有依赖

SQLite 与 MariaDB 客户端在 MCP 之前就已随后端分发：`libsqlite3-sys` 0.35（捆绑的 SQLite
公有域源码）经 `rusqlite` 引入，MariaDB 走 `mysql_async` 0.37（MIT OR Apache-2.0）。它们的
许可形态与本文 §1.3 同类，本文**不**替既有泳道补写逐条声明；如果将来要出正式发行版，应当由
打包泳道用工具一次性生成全量 `THIRD-PARTY-NOTICES`（见 §3）。

## 3. 生成全量清单的做法（可复现）

本文只维护「有额外义务」的条目。需要全量清单时，在 `backend/` 下执行：

```bash
# 方式一：已装 cargo-about
cargo about generate about.toml > THIRD-PARTY-NOTICES.txt

# 方式二：无额外依赖时，至少列出直接依赖 + 版本 + 许可
cargo tree -p sqarvo-mcp -e normal | head -40
grep -A2 'name = "sqlparser"' Cargo.lock
```

工具产物（`THIRD-PARTY-NOTICES.txt`）是**发布物附件**，不提交进仓库：它随依赖漂移，
提交就会过期。真正需要人工维护的只有 §1 那几条带义务/`NOTICE` 的条目——上游换了许可，
`policy_snapshots` / 构建会先变红（[ADR 0033](decisions/0033-sql-parser-readonly-policy.md)
「重估条件」的 NOTICE/依赖检查一条），再回来改本文。

## 4. 打包侧待办（不在文档泳道内，登记在这里避免丢失）

| 目标 | 要做的事 | 现状 |
|---|---|---|
| Linux `.deb` | `packaging/linux/build-deb.sh` 把 §1 的 NOTICE 正文装进 `/usr/share/doc/sqarvo/NOTICE`，并在 `copyright` 里加一行指向它 | 未做（本次泳道只写文档，脚本属代码改动） |
| Windows 安装包 | 随附 `NOTICE.txt` 到安装目录 | 未做，依赖 #218 的 Windows CI 作业先落地 |
| macOS | 同上 | 未接入（macOS 前端尚未接后端） |
| GitHub Release 附件 | 发布说明里链接本文与生成的 `THIRD-PARTY-NOTICES.txt` | 未做，属发版流程 |

## 5. 边界声明

- 本文不是法律意见，也不改变任何依赖的许可；它只履行署名与随附义务。
- 与 Sqarvo 自身许可冲突的依赖不允许引入：AGENTS.md §15 的依赖准入确认单负责这件事，
  本文只记录准入之后的义务履行。
- 任何密码、密钥、连接串都不属于本文的范围；示例一律用占位符（见
  [MCP 工具与配置参考](api/mcp-tools.md)）。
