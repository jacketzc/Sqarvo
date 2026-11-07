# ADR 0020：MariaDB 的 verify-ca 归一化为 verify-full

- 状态：已接受
- 日期：2026-11-07
- 关联：Issue #107（里程碑 M6）；#106（TLS 逐档实测）；ADR 0011（MariaDB 驱动）

## 背景

`TlsMode` 有五档：`disabled` / `preferred` / `required` / `verify-ca` / `verify-full`。两档证书
校验档的**意图**不同：

- `verify-ca`：把证书链校验到给定根证书，**不**校验主机名；
- `verify-full`：校链 ＋ 校主机名。

#106 在 MariaDB 11.4.13 上逐档实测了五档握手（判据是服务端会话变量 `Ssl_cipher` 是否非空，
8 个用例，全部由环境变量门控），`verify-ca` 的行为与这一档的意图不符：对容器自动生成的自签
证书（`CN=MariaDB Server`，无 `subjectAltName`）用 `verify-ca` ＋ 固定根证书连接
`127.0.0.1`，仍然被拒绝——

```
InvalidCertificate(NotValidForNameContext { expected: IpAddress(127.0.0.1), presented: [] })
```

同一张证书在 `verify-full` 下同样被拒：两档在无 SAN 证书上的表现完全一致。也就是说
`verify-ca` 事实上一直在做主机名校验，只是文档里没写。

## 上游根因

`mysql_async` 0.37.1 的 rustls 后端（`src/io/tls/rustls_io.rs`）用一个自定义
`DangerousVerifier` 包装 `WebPkiServerVerifier`，靠**字符串匹配**决定是否跳过名字校验：

```rust
if e.to_string().contains("NotValidForName") { /* 当作名字不匹配，放行 */ }
```

而 rustls 0.23 的 `Display` 输出是 `invalid peer certificate: certificate not valid for name …`，
永远不包含 `NotValidForName`——那是枚举变体的名字，不是展示文案。于是
`SslOpts::with_danger_skip_domain_validation(true)` 成了死参数：我们打开了它，上游仍然校名。

结论：这不是配置问题，也不是我们的映射写错。在当前依赖组合下 `verify-ca` 拿不到「只校链、
不校名」的行为，而且偏向更严的一侧（拒绝连接，而不是放行错误证书）。

还有一处同源的映射缺口：证书校验失败经 `tokio-rustls 0.26`
（`src/common/mod.rs`）包成 `io::Error::new(ErrorKind::InvalidData, rustls::Error)`，
`mysql_async` 归到 `IoError::Io`，因此既有的 `IoError::Tls(_) → 「TLS 握手失败」` 分支命不中，
证书被拒时报的是「数据库连接中断」这类与事实无关的文案；`required` 撞上不支持 TLS 的服务端时，
`mysql_async` 给出的是 `DriverError::NoClientSslFlagFromServer`（`display`：
`Client asked for SSL but server does not have this capability`），旧映射没有这个变体分支，
落到兜底的 `Internal`。

## 备选方案

### A. 升级 `mysql_async` / 等上游修好

- 优点：零自研安全代码，档位语义回到文档承诺的样子。
- 缺点：（1）修复要等上游把判断从字符串改成按 `rustls::Error` 枚举匹配，时间不可控；
  （2）在修好之前，缺口只能靠「文档里写一句」掩盖，属于静默退化，也正是本 ADR 要禁止的形态；
  （3）即便上游修好，我们仍然需要一个能力位来表达「这个驱动的 verify-ca 是不是真的
  chain-only」——否则换一次依赖版本就悄悄改变安全语义。
- 结论：不作为本次的处置手段；能力位部分与本方案不冲突，已并入决策。保留为「重新评估条件」
  的第一条。

### B. 自写 `ServerCertVerifier` 实现 chain-only

- 优点：语义与契约完全一致，用户拿到真正的一档。
- 缺点：（1）rustls 的 `WebPkiServerVerifier` 恒定做名字校验，要跳过必须自己实现
  `ServerCertVerifier`（`verify_server_cert` 整段逻辑，安全关键代码，写错的方向可能是「放行」）；
  （2）`mysql_async` 对外只暴露 `SslOpts` 的布尔开关，没有注入自定义 verifier 的接口，
  要做得 patch / vendor 上游 crate；
  （3）项目刚被上游一次**展示文案**变更打穿，不宜再把正确性押在它的内部结构上。
- 结论：**否决**（安全面与维护面的代价都高于收益）。

### C. 归一化并显式声明（本决策）

- 把 MariaDB 驱动的 `verify-ca` 归一化为 `verify-full`；
- 新增能力位，让前端与用户看得见「这一档在本驱动上没有 chain-only」；
- 同一 PR 内按真实原因修正证书类错误的文案与分类。
- 优点：不写安全关键代码、不改契约、退化方向只会更严、退化可见。
- 缺点：MariaDB 上确实拿不到 chain-only，这类用户需要改证书或改校验名（见「代价」）。

## 决策

### 1. `verify-ca` 归一化为 `verify-full`

MariaDB 驱动构造 `SslOpts` 时把 `verify-ca` 与 `verify-full` 走同一条路径（固定根证书 ＋
主机名校验）。**契约不变**：`TlsMode` 仍是五档，`VERIFY_CA` 的定义仍是「只校链、不校名」；
变的是「这个驱动实现这一档的方式」。

**为什么写「归一化」而不是「等效」**：

- 「等效」描述的是**契约里两档本身**的关系，读起来像 `VERIFY_CA` 与 `VERIFY_FULL` 在定义上
  就是同一件事。那是错误结论，下一个驱动照着抄就会把契约层的区分抹掉。
- 「归一化」描述的是**实现动作**：驱动收到 `VERIFY_CA` 后把它归到一个更严的档去执行。它同时
  保住三个事实——档位定义没变、退化是驱动侧的选择、将来有驱动真做到 chain-only 时这一档会
  恢复原义。
- 同一条理由支配用户可见文案：ADR 与驱动文档统一写「归一化为 verify-full」；GTK 连接对话框
  因为空间有限，标注成「校验 CA（verify-ca，本驱动等同 verify-full）」，那里的「等同」只描述
  用户能观察到的结果，不承担契约层的定义职责。

### 2. 硬规则：只允许向更严的方向退化

驱动实现不了某一档时：

- **允许**向更严的档退化（`verify-ca` → `verify-full` 这类收紧）；
- **禁止**向更松的档退化（把校验档降成 `required`、把 `required` 降成 `preferred`、把任何
  校验档降成不校验证书）；
- **必须显式**：退化同时出现在能力位与文档里，不得只写在代码注释或发布说明的脚注。没有能力位
  的时代，用户只能靠「连不上」或读源码发现退化——#106 之前就是这个状态。

这条规则适用于所有驱动，不只 MariaDB；`verify-ca` 归一化是它的第一个实例。

### 3. 能力位声明

- `driver-api` 的 `Capabilities` 新增正向能力位 `tls_verify_ca_chain_only: bool`，含义是
  「本驱动支持只校链、不校名」，默认 `false`（不实现就不声明，与 `Capabilities::default()`
  的保守习惯一致）；
- 经 gRPC 以字符串形式出现在 `repeated string capabilities` 里（`SystemService.ListDrivers`
  与 `ConnectionService.ConnectResponse`），名字是 `tls_verify_ca_chain_only`。与
  `row_editing` / `parameterized_statements` 同一机制（`Capabilities::names()`），前端不必为它
  新增字段；
- MariaDB 该位为 `false`；SQLite 同为 `false`（本地文件库没有 TLS 档位，该位对它没有语义）；
- 前端据此在连接对话框把 `verify-ca` 档标注为「校验 CA（verify-ca，本驱动等同 verify-full）」，
  选中该档时行副标题为「本驱动无法只校验证书链：仍会校验主机名」。`tls_server_name` 这个出路
  写在驱动文档与错误文案说明里——连接对话框当前没有该字段，不在此处承诺。将来支持 chain-only
  的驱动置 `true`，标注自动消失——前端不需要识别驱动名。

### 4. 错误文案与分类：强类型判定，不做字符串匹配

`tokio-rustls 0.26` 用 `io::Error::new(ErrorKind::InvalidData, rustls::Error)` 包装握手错误，
所以 `mysql_async` 的 `IoError::Io(io)` 可以用 `io.get_ref().downcast_ref::<rustls::Error>()`
取回强类型，再按 `CertificateError` 变体分类。错误码都是 `mariadb.connection_failed`：

| 情形 | rustls 变体（举例） | 文案 | 分类 |
|---|---|---|---|
| 证书主机名不匹配 | `NotValidForName` / `NotValidForNameContext` | 服务端证书与主机名不匹配 | `Unavailable` |
| 证书不受信 / 已失效 / 用途不符 | `UnknownIssuer`、`Expired` / `ExpiredContext`、`InvalidPurpose*`、`BadSignature` | 服务端证书未被信任或已失效 | `Unavailable` |
| 其他 TLS 错误 | — | TLS 握手失败（既有 `IoError::Tls` 分支） | `Unavailable` |
| `required` 撞上不支持 TLS 的服务端 | — | 服务端不支持 TLS（连接要求 TLS） | `Unavailable`（原 `Internal`） |

`Unavailable` 的理由：这类失败是「当前连不上」，改配置或重试后可能成功，不是服务端内部错误。

**归一化的一个直接后果**：无 SAN 的证书在 `verify-ca` 下与 `verify-full` 一样被拒，落到上表
第一档。文案必须写「主机名不匹配」而不是「证书无效」，否则用户会去换证书，而真正该换的是
校验名（`tls_server_name`）。

### 5. 明确不变的部分

- 五档 `TlsMode` 的语义与 proto 定义不变；
- 默认档位不变：`Unspecified` → `preferred`，即**不校验证书**。这是维护者明确保留的现状，
  属于已知风险（见「风险」），本 ADR 不声称已修；
- `preferred` 在服务端无 TLS 时的明文回退与 `mariadb.tls_fallback` 告警不变。

## 代价

- **放弃 chain-only 的代价**：证书缺少与连接地址匹配的 SAN、而用户又想校链时，MariaDB 上只能
  用 `verify-full` 并让校验名匹配证书里的某个名字——既有字段 `tls_server_name` 就是出口
  （#106 实测：`SQARVO_MARIADB_SAN_SERVER_NAME=localhost` 时
  `verify_full_accepts_matching_certificate` 通过，走证书的 `DNS:` SAN）。证书既没有 SAN、
  也给不出可校验名字时，MariaDB 上就没有「只校链」这一档：用户只能改用不校验证书的档位或换
  证书。这是真实的能力缺失，不是文案问题。
- **用户预期落差**：从别的客户端迁过来的人会认为 `verify-ca` 天然不校名。UI 标注只能降低误解，
  不能消除；报错文案「主机名不匹配」是第二道防线。
- **多一个能力位与一段前端标注逻辑**：旧客户端不认识这个字符串，会忽略它（`repeated string`
  追加不破坏兼容），于是旧前端的 `verify-ca` 只显示原名、少一句说明。可接受：能力位不是协议
  必需字段，行为本身仍然安全。

## 风险

- **默认档位不校验证书**：`Unspecified` 落到 `preferred`，连接成功即接受任意证书（含自签、
  含中间人）。归一化让**显式选择** `verify-ca` 的用户更安全，但没有改变默认值。这是已知风险，
  本 ADR 只登记，不处置。
- **与 rustls 版本的类型耦合**：错误分类依赖「`io::Error` 内层是 `rustls::Error`」这一包装约定。
  上游若改成字符串或自定义错误类型，`downcast_ref` 会失败并回落到通用 TLS 文案——分类仍是
  `Unavailable`，不会把「主机名不匹配」误报成「证书不受信」，但文案精度会退。升级
  `rustls` / `tokio-rustls` 时必须重跑证书类用例。
- **再次依赖上游内部行为**：本决策只关闭了「结果差异」（归一化），并没有恢复对
  `skip_domain_validation` 的依赖，但仍读 `mysql_async` 的错误变体结构。任何「按上游展示
  文案判断」的代码今后一律不接受。

## 已评估、未采纳

**连接期发 `mariadb.tls_mode_degraded` 运行时告警**（像 `mariadb.tls_fallback` 那样，在 `Connect`
的 warnings 里说明「verify-ca 已归一化为 verify-full」）。

未采纳的原因：`DriverSession` 目前没有 warnings 通道——`connect` 只返回
`Box<dyn DriverSession>`，只有 `TestConnection` 的 report 带 warnings。要做就得改 SPI 签名
（`connect` 返回值或 `DriverSession` 增方法），为一个按驱动固定的静态退化付这个代价不划算；
静态能力位 ＋ 连接对话框标注已经能在**连接之前**把话说清，而 `tls_fallback` 是运行时才知道的
结果（服务端到底支不支持 TLS），两者的信息时机不同。若将来 `DriverSession` 因别的需求有了
warnings 通道，这件事可以顺手补上。

## 验证

- 行为取证（#106，2026-11-06）：`crates/drivers/mariadb/tests/tls_handshake.rs` 的 8 个用例，
  判据是服务端会话变量 `Ssl_cipher` 非空；`verify_ca_trusts_pinned_root_certificate` 在带 SAN
  的监听端通过，`verify_full_rejects_hostname_mismatch` 在无 SAN 证书上给出
  `NotValidForNameContext`。运行命令与真实输出见 `docs/database/mariadb.md`「TLS 逐档验证」。
- 本 ADR 的实现验证：`crates/drivers/mariadb/tests/tls_handshake.rs` 新增
  `verify_ca_is_normalized_to_verify_full`（对无 SAN 证书在 `verify-ca` 下断言
  `mariadb.connection_failed` / `Unavailable` 与「服务端证书与主机名不匹配」），
  `required_fails_on_server_without_tls` 改断言 `Unavailable` 与新文案。能力位的声明与
  `Capabilities::names()` 输出由 `driver-api` 的单测
  `tls_verify_ca_chain_only_is_a_positive_capability` 锁定；GTK 侧的标注逻辑由
  `verify_ca_label_marks_degradation_only_without_chain_only_capability` 锁定（能力串含该位时
  标注自动消失，不按驱动 ID 硬编码）。
- 本地执行（2026-11-07）：三个监听端（53306 自签自动证书、53307 `--skip-ssl`、53308 自建 CA
  签的带 SAN 证书）上 `cargo test -p sqarvo-driver-mariadb --test tls_handshake -- --nocapture
  --test-threads=1` → **9 passed / 0 failed**；backend `cargo test --workspace` 通过、frontend/linux
  `cargo test` → **197 passed**；两个 workspace 的 `cargo clippy --all-targets -- -D warnings`
  均无 warning。TLS 用例是环境变量门控的本地证据（根证书由 `openssl s_client -starttls mysql`
  从 53306 的握手取出），CI 尚未接入。

## 重新评估条件

- 上游 `mysql_async` 把名字校验的判断改成按 `rustls::Error` 枚举匹配，或提供自定义 verifier
  注入接口 → 撤掉归一化，能力位置 `true`，前端标注自动消失；
- 出现必须 chain-only 的真实用户场景（内部 CA 签的证书只能按 IP 连接、又必须校链）→ 回到
  方案 B（自写 verifier ＋ 上游 patch），届时单独立 ADR 并做安全评审，不能只翻一个布尔；
- 某个驱动只支持五档的子集（例如完全不支持 `verify-ca`）→ 能力位从 `bool` 升级为逐档声明
  （在 `repeated string` 里列支持的档位），`bool` 只够表达「chain-only 有没有」；
- `rustls` / `tokio-rustls` 升级导致 `io::Error` 内层类型或包装方式变化 → 先补/改证书类用例，
  再改错误映射。
