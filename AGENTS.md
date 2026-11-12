# AGENTS.md

This document defines the AI agent development guidelines for the repository root. It applies to code-generation agents, review agents, and automated development tools working in this repository. These guidelines apply to the entire repository unless a subdirectory contains a more specific `AGENTS.md`. Subdirectory guidelines must not violate the product goals, security boundaries, or architectural constraints defined here.

## 1. Project Positioning

This project is an **open-source, lightweight, cross-platform, native desktop database management client**. Its goal is to provide developers with everyday database management capabilities comparable to mature commercial database clients, while maintaining low resource usage, fast startup, and a high-quality native desktop experience.

Core positioning:

- Open source, community-friendly, and engineer-led.
- Cross-platform support for Windows, Linux, and macOS.
- Use the corresponding native desktop UI technology on each platform.
- Do not use Electron, Chromium, CEF, or WebView as the application UI runtime.
- Centralize database business capabilities in an independent Rust backend process.
- Use gRPC + Protocol Buffers for frontend/backend communication.
- Integrate different databases through an extensible Driver SPI.
- Lightweight does not mean feature-reduced. Performance, feature completeness, and maintainability must be considered together.

The project may refer to publicly documented features and user workflows of mature database clients such as Navicat. However, it must be designed and implemented independently. Do not copy proprietary code, UI assets, trademarks, icons, text, resource files, or non-public implementation details. Do not imply that this project is officially affiliated with Navicat.

## 2. Established Technology Stack

### 2.1 Desktop Frontends

| Platform | UI Technology | Primary Language | Constraints |
|---|---|---|---|
| Windows | WinUI 3 / Windows App SDK | C# | Use native Windows desktop application technology. Do not introduce WebView2 or Chromium. |
| Linux (Ubuntu / GNOME as the primary desktop target) | GTK4 + libadwaita | Rust (gtk-rs) | Follow GNOME HIG. Do not assume that every Linux desktop environment is GNOME. |
| macOS | SwiftUI, with AppKit when necessary | Swift | Prefer native system controls, menus, keyboard shortcuts, and system services. |

Frontend implementations must follow each platform's design conventions. The three platforms should maintain **consistent functionality and core interaction semantics**. Pixel-level consistency is not required, nor is forced sharing of UI code.

### 2.2 Unified Backend

- Language: Rust.
- Runtime model: an independent companion process for the desktop application (Backend Service).
- Async runtime: prefer Tokio. If another runtime is introduced, explain why it is necessary and define its boundaries relative to the existing async model.
- Frontend/backend communication: gRPC.
- API contract: Protocol Buffers (Protobuf).
- Database access: abstract through the project's own Driver SPI. Evaluate concrete driver libraries database by database. Do not leak a particular driver framework into public business interfaces.

### 2.3 Communication and Deployment

The default operating mode for the first phase is **a local frontend and backend running as separate processes**. Remote backends, team collaboration, remote agents, and similar capabilities are future extensions. Unless explicitly required by a product requirement, do not over-engineer the first release for remote operation that is not yet planned.

Local communication must restrict access:

- Linux/macOS: evaluate Unix Domain Sockets first and set strict file permissions.
- Windows: evaluate Named Pipes with ACLs first. If the current gRPC client/server combination cannot support them reliably, a temporary alternative is TCP bound only to `127.0.0.1`, protected with authentication measures such as a random session token.
- Never listen on `0.0.0.0`, a public address, or a LAN address by default.
- Never expose backend management interfaces without authentication.
- Hide transport-specific implementation behind a Transport abstraction. Business layers must not depend on the concrete Socket, Pipe, or port implementation.

## 3. Product and Experience Principles

### 3.1 Product Experience

- Startup should be fast and idle resource usage should be low. Performance optimizations must be based on measurements; do not sacrifice maintainability based on intuition alone.
- Common operations should be keyboard-accessible. Shortcuts should follow platform conventions, while product specifications may define consistent cross-platform action semantics.
- Support multiple connections, database object browsing, and multiple SQL workspaces.
- SQL editing, query results, and table data browsing are core workflows. Do not treat them as ordinary form pages.
- Long lists and large result sets must use virtualization, pagination, or batched retrieval. Do not load all data at once by default.
- The UI must clearly communicate loading, empty data, errors, cancellation, read-only state, transaction state, and disconnection.
- Operations that may modify data or have irreversible effects should receive warnings proportionate to their risk. Do not add meaningless confirmation dialogs to every ordinary query.

### 3.2 Feature Scope

The scope of each release is governed by requirements and the roadmap under `docs/product/`. Capabilities may generally be developed in the following order:

1. Connection configuration, connection testing, connect/disconnect, and session management.
2. Database object tree and metadata browsing.
3. SQL editor, syntax highlighting, execution, cancellation, and query history.
4. Query result display, pagination/batched retrieval, and export.
5. Table data browsing, editing, and safe change submission.
6. Table structure inspection, DDL operations, and database-specific capability display.
7. Advanced capabilities such as import/export, schema comparison, data migration, and backup/restore.

Do not implement placeholder features without clear requirements, tests, or error handling merely to increase the feature count. Unimplemented capabilities must be clearly labeled and must never be presented as supported.

## 4. Architecture Boundaries

### 4.1 Frontend Responsibilities

The frontend is responsible for:

- Native windows, navigation, menus, workspaces, dialogs, and user interaction.
- Rendering the SQL editor, result tables, and database object tree.
- Basic validation of user input and interaction feedback.
- Calling gRPC APIs and handling cancellation, progress, pagination, and backend lifecycle.
- Platform integration: system credential stores, file pickers, notifications, system tray, keyboard shortcuts, and installer integration.

The frontend must not:

- Implement database network protocols or connect directly to user databases.
- Duplicate database drivers, connection pools, or SQL execution logic across platforms.
- Bypass gRPC to access internal backend database modules directly.
- Perform database I/O, large-scale file processing, or other long-running work on the UI thread.
- Write database passwords to ordinary configuration files, logs, crash reports, or telemetry data.

### 4.2 Rust Backend Responsibilities

The backend is responsible for:

- Database connections and session lifecycle.
- SQL execution, query cancellation, and result-set management.
- Transaction management and database capability detection.
- Database metadata, schemas, and object information.
- Data import/export and future migration capabilities.
- Driver SPI, database dialects, and database-specific adaptations.
- Configuration loading, logging, service health status, and resource cleanup.

The backend must not contain platform UI logic or depend on WinUI, GTK, SwiftUI, or AppKit.

### 4.3 Shared Contracts

- Protobuf is the single source of truth for frontend/backend communication contracts.
- Protobuf/gRPC code for each language must be generated from the contracts. Generated files must not be edited manually.
- The frontend may perform immediate input validation, but the backend must validate permissions, parameter ranges, resource states, and operation validity again.
- Do not expose Rust internal structs, Trait objects, or third-party driver types directly through inter-process protocols.
- API changes must account for compatibility, error semantics, cancellation behavior, and behavior observed by older client versions.

## 5. Recommended Repository Structure

The following is a suggested structure. Directories may be adjusted to match the actual implementation, but responsibility boundaries must remain clear. Significant structural changes should be reflected in the architecture documentation first.

```text
.
├── AGENTS.md
├── README.md
├── LICENSE
├── CONTRIBUTING.md
├── SECURITY.md
├── .github/
│   ├── workflows/            # CI
│   ├── ISSUE_TEMPLATE/       # Task / bug issue templates
│   └── pull_request_template.md
├── docs/
│   ├── product/              # Vision, positioning, requirements, roadmap
│   ├── planning/             # Development plan, milestones, release plan, risks
│   ├── architecture/         # Overall architecture, process model, data flow
│   ├── api/                  # gRPC / Protobuf design notes
│   ├── database/             # Driver SPI, dialects, and database differences
│   ├── ui-specification/     # Interaction specifications, UI flows, design principles
│   └── decisions/            # ADRs: key technical decisions and rationale
├── proto/
│   ├── common/v1/
│   └── services/v1/
├── frontend/
│   ├── windows/              # WinUI 3 + C#
│   ├── linux/                # GTK4 + libadwaita + Rust
│   └── macos/                # SwiftUI + AppKit + Swift
├── backend/
│   ├── Cargo.toml            # Rust workspace
│   ├── apps/
│   │   └── backend-service/
│   └── crates/
│       ├── application/
│       ├── connection/
│       ├── query-engine/
│       ├── metadata/
│       ├── transaction/
│       ├── driver-api/
│       ├── drivers/
│       ├── rpc-server/
│       ├── local-store/
│       └── common/
├── tests/
│   ├── contract/
│   ├── integration/
│   └── fixtures/
└── packaging/
    ├── windows/
    ├── linux/
    └── macos/
```

Database drivers are workspace crates under `backend/crates/drivers/<database>/`. Do not create a
separate top-level `database-drivers/` tree: the Rust workspace is the build, test, and dependency
unit, and a second location would split driver ownership across two build systems.

## 6. Database Driver SPI Guidelines

### 6.1 Core Principles

- Use a project-owned, stable Driver SPI that is as independent as possible from concrete driver libraries.
- Initially, prefer compiling built-in drivers into the Backend as Rust workspace crates.
- Do not introduce a Rust dynamic-library plugin ABI at the beginning. Rust does not provide a stable cross-version ABI that can be relied upon directly. Out-of-process plugins are a future design option.
- Keep each database's connection, metadata, SQL dialect, transaction, and specialized capabilities within clearly defined implementation boundaries.
- The shared interface should express only capabilities that are genuinely reusable across databases. Database-specific capabilities should be exposed through capability descriptors or typed extensions.

### 6.2 Do Not Assume All Databases Are Equivalent

Concepts and behaviors differ across databases. For example, Database, Catalog, Schema, Namespace, Owner, Sequence, and Package cannot be collapsed into one ambiguous field.

When designing shared metadata models:

- Preserve object types, original type names, fully qualified names, and necessary database-specific information.
- Do not lose field precision, length, signedness, time-zone information, character set, collation, default values, or other relevant metadata.
- Explicitly return an unsupported status or capability state for unsupported features. Never silently simulate success.
- SQL dialects and DDL must be handled by the corresponding driver. Do not scatter database-name conditionals throughout generic business logic.

### 6.3 Data Types and Result Sets

- Do not represent every database value as a `String`.
- Support NULL, booleans, signed/unsigned integers, floating-point values, Decimal, text, binary data, dates, times, timestamps with time zones, JSON, arrays, and database-specific values.
- Precision-sensitive types such as Decimal must not be converted to floating-point values unconditionally.
- Design deferred or chunked retrieval for binary large objects and very large text values.
- When a value cannot be represented losslessly, preserve its original type information and use an explicit display/export policy.

### 6.4 Driver Lifecycle

Each driver must define at least:

- Connect, disconnect, connection testing, and error classification.
- Session ownership and rules for concurrent use.
- Query execution, query cancellation, result-set closure, and resource cleanup.
- Transaction support and isolation-level capabilities.
- Metadata retrieval and pagination strategy.
- Driver version, supported database version range, and capability declarations.

Do not assume every driver supports identical asynchronous behavior. Blocking drivers must use an appropriate blocking task pool and must not block critical Tokio runtime worker threads.

## 7. gRPC / Protobuf Guidelines

### 7.1 API Design

Split services by business domain. Avoid putting every operation into one ever-growing universal service. Suggested domains include:

- ConnectionService
- MetadataService
- QueryService
- TransactionService
- DataTransferService
- SystemService

The actual service and RPC names are governed by the official definitions under `proto/`. Before adding a service, determine whether the existing domain service can be extended.

### 7.2 Protobuf Compatibility

- Use explicit package and version names, for example `product.query.v1`.
- Never reuse a field number after it has been published. Mark deleted fields as `reserved`.
- Avoid unnecessary breaking changes to field types.
- Define sensible default semantics for newly added fields.
- Error responses must use stable error codes/categories and include safe information suitable for display to users. Never return database passwords, secrets in connection strings, or internal stack traces to the UI.
- Define progress, cancellation, and terminal-state semantics for long-running operations.
- After modifying Protobuf definitions, regenerate all affected client code and run contract tests.

### 7.3 Query Result Transport

Never serialize and return an entire large result set to the frontend in one response by default.

Distinguish between two access patterns:

1. **SQL query results**: support Query IDs, result-set metadata, batched retrieval or streaming, cancellation, resource closure, and backpressure.
2. **Table browsing**: prefer pagination/cursor-based retrieval; declare sorting and filtering capabilities, and define pagination stability and ordering rules.

Also consider:

- Multiple statements, multiple result sets, and update counts.
- Column types, precision, nullability, and display hints.
- Deferred retrieval of very large fields.
- Task cleanup after client disconnection.
- Cleanup of unreleased Sessions, Cursors, and temporary files when the service exits.
- Reasonable limits for message sizes, concurrent queries, and buffers.

Do not solve large-result-set problems by simply increasing the gRPC message-size limit without bounds.

### 7.4 Cancellation and Errors

- User cancellation of a query must be propagated to the database driver as far as possible.
- Cancellation must be idempotent. Repeated cancellation must not crash the process.
- Distinguish connection loss, query failure, timeout, cancellation, insufficient permissions, and unsupported capabilities.
- Error messages must be diagnosable without leaking credentials, full sensitive connection strings, or private local paths.

## 8. Native Frontend Guidelines

### 8.1 General Requirements

- Do not use Electron, Chromium, CEF, WebView2, WKWebView, WebKitGTK, or any other embedded web runtime as the UI implementation.
- Do not imitate a native interface using HTML/CSS/JavaScript pages.
- Implement business interactions with native components. Reuse of pure algorithm libraries or generated protocol code is acceptable when necessary, but Web UI must not replace desktop UI.
- Do not break platform interaction conventions in pursuit of pixel-level consistency across the three platforms.
- Core operation semantics, states, error messages, and data content must remain consistent.
- **Prefer the platform's own controls.** Every new surface starts from a native control (menus, popovers, dialogs, tables, tabs, pagination, toggles). Four levels: **L0** native control with default styling; **L1** native control styled with theme tokens (its own cells and rows included); **L2** a self-drawn visual, or native controls hand-assembled into a composite the toolkit does not ship — allowed, but it must be registered in the component registry with `hand_drawn` / `why_not_native` (registration is enough, no ADR); **L3** hand-implementing what the toolkit already provides (own menu, own dialog, own table, own tab strip) — forbidden, an ADR is required before implementing.
- **An unavailable item is not a greyed placeholder.** Capability-level unavailability (this connection, this driver or this platform does not support it at all) must not appear: the item is not listed, the button is not rendered. State-level unavailability (nothing selected, no dirty rows, read-only, RPC in flight) stays visible and disabled with the reason in a tooltip / status line, because the user can make it available in one step. The one existing exception is the object tree's "not implemented" group rows (behavior contract §2.1 keeps them visible on purpose).
- Cross-platform consistency is **behavior**, not appearance: items, order, availability, shortcuts and copy must match; separators, icons and type sizes follow each platform's conventions.

### 8.2 Windows

- Use WinUI 3 and Windows App SDK.
- Write UI code in C# and communicate with the Backend through generated gRPC clients.
- Follow Windows conventions for windows, menus, keyboard input, themes, DPI, file selection, and accessibility.
- Do not introduce WebView2 by default merely to implement a complex control.

### 8.3 Linux

- Use GTK4, libadwaita, and gtk-rs.
- GTK UI updates must run on the GTK main thread.
- Database requests, file processing, and other long-running tasks must run in the background, with results safely dispatched to the main thread.
- Do not block the GTK main loop.
- libadwaita components should follow GNOME HIG. Provide reasonable behavior in non-GNOME desktop environments; do not assume users always run GNOME Shell.
- Implement file, credential, theme, and desktop integration through a Linux platform-adaptation layer.

### 8.4 macOS

- Use SwiftUI as the primary UI framework. Use AppKit for complex desktop controls when there is a clear reason.
- Follow macOS conventions for menus, keyboard shortcuts, windows, toolbars, file dialogs, and system credential management.
- Use Swift Concurrency or an appropriate asynchronous mechanism. Do not perform blocking database operations on the main thread.
- Isolate generated gRPC code behind a communication layer so the UI does not directly depend on low-level transport details.

### 8.5 SQL Editor and Data Grid

These controls are core product components. Do not substitute a simple multiline text field or ordinary list for production functionality.

The SQL editor should progressively support:

- Large text input and efficient updates.
- SQL syntax highlighting and multiple dialects.
- Line numbers, current line, selection, and indentation.
- Find/replace.
- Multi-statement execution and execution of the current statement.
- Autocompletion and metadata hints, introduced progressively.
- Responsiveness with large files and long text.

The data grid should progressively support:

- Virtualization or on-demand rendering.
- Pagination/batched retrieval; do not load all data into memory by default.
- Column resizing, sorting, filtering, and copying.
- Correct display of NULL, binary data, dates, Decimal, JSON, and other types.
- Editing state, dirty-data indicators, commit, and rollback.
- Expansion/deferred loading of large fields.
- Explicit read-only and editable states.

Before adopting a specific control library or editor component, verify its license, cross-platform support, accessibility, maintenance status, and performance with large data volumes. Do not introduce dependencies based solely on popularity.

### 8.6 Sizing, Density Scaling, Theme and Localization (binding, all three platforms)

The single source of truth for every size number is `docs/ui-specification/metrics.toml`; the rules
are in `docs/ui-specification/scaling-and-window-sizing.md`; the decision is
[ADR 0026](docs/decisions/0026-ui-scaling-window-sizing.md).

- Never hardcode a second copy of a size. Platform code reads `metrics.toml` (Linux parses it at
  runtime; Windows/macOS generate constants at build time) and must fail the build when generation
  fails. Do not compute `base × scale` in code: Rust, C# and Swift round differently and drift by 1px.
- Exactly three density tiers exist: `compact` 100% (default), `comfortable` 125%, `large` 150%.
  Do not add a tier without extending `metrics.toml` and its invariant tests. Tiers below 100% are
  rejected by design (they break the 24px minimum target and the CJK 13px font floor).
- The OS DPI / scale factor is never multiplied into application metrics. GTK logical pixels and
  XAML DIPs are already density-independent; convert to physical pixels only when talking to
  `AppWindow` / `DisplayArea` / `GdkMonitor::geometry`.
- Window size is chosen from the **logical work area**, never from the screen resolution, and the
  minimum window is `min(960·T, work_area − 32) × min(640·T, work_area − 48)`. A window must never
  exceed the work area, including on 1080p@200% (logical 960×540).
- Colors come from theme tokens only (`@define-color` / `ThemeResource`). A `#RRGGBB` literal in
  interface CSS or XAML is a defect; the only allowed hex values are the Linux
  `LIGHT_PALETTE` / `DARK_PALETTE` constants in `frontend/linux/src/theme.rs`. Windows must respect
  the system accent color and high contrast; Linux must branch on `AdwStyleManager::is_high_contrast()`
  where contrast matters.
- Dark mode follows the system on all platforms. Do not implement a private light-only fallback;
  Windows sets `Application.RequestedTheme` at startup only and switches at runtime through
  element-level `FrameworkElement.RequestedTheme`, and always keeps the title bar in sync.
- New user-facing strings go through a per-crate string catalog, not inline literals, even while
  only Chinese ships. New interactive controls need an accessible name and a defined Tab position.
- Any change to these numbers or rules updates `metrics.toml`, `scaling-and-window-sizing.md`,
  and (for behavior) `docs/ui-specification/ui-behavior-contract.md` in the same PR.

## 9. Security and Privacy

Database clients frequently handle highly sensitive credentials and production data. Security is a foundational capability, not an optional enhancement.

- Prefer operating-system credential stores for passwords, tokens, private keys, and other secrets: Windows Credential Manager, macOS Keychain, Linux Secret Service, and equivalent facilities.
- Store only credential references or non-sensitive settings in ordinary configuration storage. Never store database passwords in plaintext.
- Keep credentials in memory only as long as necessary. Never write them to logs, telemetry, crash reports, or test snapshots.
- Never write full connection strings that may contain passwords to error logs.
- Validate paths and permissions for all import/export operations to prevent path traversal and accidental overwrites.
- Preserve the database's existing permission semantics during SQL execution. Do not default to connecting with privileged accounts.
- Clearly communicate the impact scope of high-risk operations such as deletion, overwrite, and bulk updates.
- Local IPC must enforce access control. Never expose it to the network without authentication.
- If remote connections are added in the future, they must use TLS and properly verify server identity.
- Do not upload SQL, database schemas, data samples, or connection information to third-party services by default.
- Any future telemetry must follow explicit user consent, data minimization, and opt-out requirements.
- When upgrading dependencies, review security advisories, licenses, and supply-chain risks.

## 10. Performance and Resource Management

- Establish reproducible benchmarks before optimizing performance.
- Set memory limits for query results, import/export, object trees, and large-file operations.
- Use streaming/batched processing to avoid unnecessary data copies and serialization.
- Every query, task, connection, cursor, and temporary file must have a defined lifecycle and cleanup path.
- Set concurrency limits, timeout policies, and cancellation mechanisms for background tasks.
- Do not use unbounded channels, unlimited task creation, or unbounded caches to create the appearance of responsiveness.
- The frontend main thread should perform only necessary UI updates.
- Do not claim "zero overhead," "extreme performance," or similar metrics without supporting measurements.

## 11. Rust Development Guidelines

- Follow `rustfmt` formatting and Clippy recommendations. If a lint must be ignored, suppress it locally and explain why.
- Public types, Traits, modules, and complex algorithms should have clear documentation comments.
- Error types should provide context and support classification. Avoid converting every error to an uninformative string.
- Avoid unnecessary `unwrap()`, `expect()`, and panics. Use them only where an invariant has been established, and explain the reasoning.
- Do not hold locks across `.await` in async tasks unless necessary and justified.
- Prefer clear ownership models for shared state. Do not introduce complex `Arc<Mutex<...>>` structures prematurely.
- Validate external input, SQL, file paths, Protobuf fields, and database metadata.
- Use `unsafe` code sparingly. Document its safety invariants and add targeted tests.
- Before adding a dependency, check its maintenance status, license, build footprint, platform support, and security history.

## 12. C# / Swift / GTK Development Guidelines

- Follow the official formatters and static-analysis tools for each language and framework.
- Organize UI, state management, gRPC clients, and platform-service adapters into separate layers.
- Do not put database business logic in Views or Widgets.
- All asynchronous operations must handle cancellation, exceptions, and callbacks that arrive after a component has been destroyed.
- UI state must be able to represent Loading, Empty, Success, Error, Cancelled, and Disconnected states.
- Do not perform synchronous network, disk, or database operations on the UI thread.
- Before adding third-party controls, verify their native integration, license, accessibility, keyboard support, and high-DPI/scaling behavior.

## 13. Testing Requirements

Before submitting code, run the checks relevant to the scope of the changes. If the environment lacks required tools or platforms, do not claim that tests passed. Clearly record which checks were not run and why.

Testing should include at least:

1. **Unit tests**: business rules, data-type conversion, SQL dialects, error mapping, and lifecycle behavior.
2. **Driver integration tests**: connections, metadata, transactions, execution, cancellation, result sets, and type mapping.
3. **Contract tests**: Protobuf generation, field compatibility, error semantics, and client/server interactions.
4. **End-to-end tests**: frontend starts the Backend, connects to a database, executes SQL, displays results, cancels tasks, and cleans up on exit.
5. **Performance tests**: large result sets, wide tables, large fields, concurrent queries, and long-running scenarios.
6. **Security tests**: log redaction, credential storage, IPC access control, path validation, and error-message leakage.

Use dedicated test databases and synthetic data. Never connect to real production databases or use users' real credentials in tests.

If a platform cannot currently run tests, test shared protocols/business logic on available platforms and clearly mark that the affected platform has not yet been validated.

## 14. Documentation and Decision Records

Update documentation whenever any of the following changes:

- Architecture boundaries, process model, or module responsibilities.
- Addition/removal of a database driver or changes to supported database scope.
- gRPC / Protobuf APIs.
- Credential storage, IPC, or security policies.
- Platform minimum OS versions, packaging methods, or dependencies.
- Features and interactions that affect user workflows.

Record significant technical decisions as ADRs under `docs/decisions/`. Each ADR should describe at least: context, considered alternatives, decision, rationale, trade-offs, and conditions for reevaluation.

Add user documentation for user-visible features. Document every new configuration option, including its default value, persistence location, and security implications.

### 14.1 Documentation Layers and Single Source of Truth

| Layer | Location | Owner | Change rate |
|---|---|---|---|
| Product planning (vision, positioning, requirements, roadmap) | `docs/product/` | product owner | low |
| Development planning (plan, milestones, release, risks) | `docs/planning/` | project-management role | medium |
| Technical facts (architecture, API, driver SPI, UI) | `docs/architecture/`, `docs/api/`, `docs/database/`, `docs/ui-specification/` | architecture / module owners | with code |
| UI component registry (existence, style refs, per-platform sync) | `docs/ui-specification/components/` | UI owner | 每次改组件 |
| Decisions | `docs/decisions/` | decision author | append-only |
| Tasks, status, dependencies | GitHub Issues / Projects | development agents | high |

Rules:

- Stable planning lives in the repository; dynamic execution state lives in GitHub Issues/Projects.
- Do not maintain a second task list inside the repository, and do not duplicate planning documents in external tools (Notion, wikis). One plan, one place.
- Every task Issue must reference its module, milestone, priority, requirement ID, dependencies, and acceptance criteria; every PR must reference its Issue and milestone.
- `scripts/check-tracking.sh` (also run by `.github/workflows/tracking.yml`) enforces the milestone and Project-membership part of that rule; missing priority or module labels are reported as warnings.
- Development agents update Issues and PRs. They do not rewrite product requirements, milestones, or the development plan unless explicitly assigned that role.
- A milestone is only marked complete with recorded verification evidence (commands and results), not with a summary claim.

### 14.2 UI Behavior Contract (binding)

`docs/ui-specification/ui-behavior-contract.md` is the **single source of truth for UI behavior**
(what an action does, when it is enabled, what it reports, which keys trigger it). It is derived
from the shipped Linux client, so the code and the contract must agree at all times.

- **Any change that alters UI behavior or interaction must update that contract in the same
  commit/PR.** If the change does not alter behavior, say so explicitly in the PR description
  ("no contract change: pure refactor/visual tweak") instead of silently skipping it.
- A behavior the contract does not cover yet is not "done": add the row (action / precondition /
  result / source `file:line`) before reporting completion.
- If code and contract disagree, fix the one that is wrong; do not leave the disagreement in place.
  Real remaining gaps go to the contract's §9 "known deviations", with code-vs-doc and a
  recommendation — never silently deleted.
- Visual specification (sizes, spacing, colors) stays in `workspace-model.md` /
  `design-principles.md`; those files must point at the contract for behavior instead of
  restating it.

### 14.3 UI 组件注册表（绑定）

`docs/ui-specification/components/*.toml` 是界面组成的唯一真值（决策见
[ADR 0027](docs/decisions/0027-ui-component-registry.md)，字段与粒度见
`components/README.md`）。`docs/ui-specification/ui-components.md` 是它的生成物。

- **改 UI 之前先报组件名，并让维护者确认。** 拿到任何「改一下这个界面」的需求，先说清这次改动
  对应哪个 / 哪些组件（`id` + `display_name`，例如 `object_tree.tree.row.conn`「连接行」、
  `object_tree.ctx_menu.refresh_group`「刷新这一组」），请维护者确认范围后再动手。这不是形式：
  `id` 是 PR、Issue、QA 断言与三端同步标记共用的键，维护者只有反复看到同一个名字才能形成记忆；
  省掉这一步，同一个东西迟早会在三处叫三个名。
- 若改动落在还没登记的功能区（P0–P1 迁移期），先说明「这个区还没进注册表」并指出它将来归哪个区、
  哪一级，再动手；不要临时自造 id。
- 新增或改动用户可见的组件，必须在同一 PR 里改注册表。组件 `id` 发布后不改名；
  `display_name` 可以随文案演进。
- 样式只写 `metrics.toml` 的 key 名与主题 token 名。组件里出现 `px` 数字或 `#RRGGBB`
  一律算缺陷（与 §8.6 同一条规则）。
- `sync.<plat>.state = "done"` 必须带当前 `spec_hash`。改规范会让三端标记**自动失效**；
  恢复 `done` 要按新规范重新核对，再跑 `scripts/check-ui-sync.sh mark`。
  `partial` / `na` / `blocked` 必须写清差异，不接受「未同步」四个字。
- `ui-components.md` 不许手改：CI 会跑 `gen` 并与工作树比对，有差异即失败。
- 九个功能区已全部在册（186 个组件）。旧契约的功能章节尚未退役，行为叙述仍以它为准；
  改行为要同时改契约本节与注册表，直到 P3 删除旧章节。
- 未实现的平台能力记 `sync = "blocked"` 并进行为契约 §9，不得只标「未同步」。
- **「这版不要它了」用生命周期，不用「禁用」。** `status = "removed"` + `removed_in`（如 `v0.3.0`）
  + `removed_reason`；还在用但下版会被取代 / 移除用 `deprecated`。**不许直接删条目** —— 别的端要靠它
  知道「要去掉什么」；三端都清干净后才归档到 `components/_archived.toml`。退役组件的 `sync` 只写
  `removed`（该端已清：代码删了或界面隐藏了）/ `present`（还留着）/ `na`，且不记 `spec_hash`。
  单端不做记 `sync.<plat> = "na"`，那不算全局退役。查询用 `scripts/check-ui-sync.sh pending`。

## 15. Agent Workflow

Follow this sequence when executing a task:

1. Read this file and any other `AGENTS.md` files in the directories involved in the task.
2. Read the relevant product requirements, architecture documents, ADRs, and existing code. Do not guess the state of the project based only on the task description.
3. Confirm the requirements, scope of impact, module boundaries, acceptance criteria, and the associated Issue/milestone. For complex tasks, provide a brief implementation plan first. 涉及 UI 时先按 §14.3 报出对应的组件 `id` 与名称，得到维护者确认后再动手。
4. Prefer the smallest complete and verifiable change. Do not opportunistically rewrite unrelated modules.
5. Before changing APIs or public types, inspect all callers and compatibility implications.
6. Add or update tests, documentation, and examples.
7. Run relevant formatters, static checks, unit tests, and integration tests.
7a. Update `docs/ui-specification/ui-behavior-contract.md` for any UI behavior change (see §14.2);
    a UI change whose contract is stale is an incomplete change.
8. Review `git diff` to ensure it contains no secrets, personal data, generated artifacts, temporary files, or unrelated formatting changes.
9. Report completed work, key design decisions, test results, unfinished items, and known risks; update the Issue status and link the PR.

### 15.1 Commit Messages and Attribution

- Follow the repository commit convention: a Chinese `<module>: <description>` subject line
  (e.g. `前端：…`, `文档：…`, `契约：…`), describing the change rather than the process.
- Do not add `Co-authored-by:`, `Signed-off-by:`, or any other attribution trailer naming a third
  party unless that person actually authored part of the change and agreed to be credited.
- Never credit a generic tool or agent identity (for example
  `Co-authored-by: Hermes Agent <hermes@nousresearch.com>`). GitHub attributes a commit to a
  GitHub account whenever a trailer's email matches a verified email of that account, so a generic
  address silently credits an unrelated account: it appears under Insights → Contributors for this
  repository even though it has no write access, no pushes, and no PRs. Verify the mapping with
  `gh api graphql` querying `Commit.authors { email user { login } }` before assuming a stranger
  contributed code.
- Because published commits cannot be re-attributed without rewriting history (and that requires
  temporarily disabling branch protection on `main`), a mistaken trailer is effectively permanent.
  Do not rewrite `main` to remove one without explicit approval from the maintainer.

### Agent Prohibitions

- Never fabricate command output, test results, database connection results, or API behavior.
- Do not rebuild existing modules without first reading the existing code and documentation.
- Do not modify user data, production databases, system services, network configuration, or credentials without authorization.
- Do not delete unknown files, overwrite user work, or perform destructive database operations.
- Never commit real passwords, tokens, private keys, certificates, or sensitive connection strings.
- Never add attribution trailers (`Co-authored-by:`, `Signed-off-by:`, …) that credit a third party
  or a generic agent identity; see §15.1.
- Never edit generated Protobuf/gRPC files manually.
- Never bypass the Driver SPI by hardcoding a database implementation in the frontend or generic business layer.
- Never introduce Electron, Chromium, or WebView to achieve superficial cross-platform support.
- Never describe unimplemented, untested, or UI-placeholder functionality as completed.
- Do not introduce a new runtime, framework, or large dependency without a clear requirement.
- Do not create competing copies of plans or task lists outside their designated locations
  (`docs/product/`, `docs/planning/`, GitHub Issues/Projects).
- Do not mark a milestone or requirement complete without recorded verification evidence.

## 16. Task Handoff Template

When completing a development task, the final report must include at least:

- **Completed**: what was implemented and which modules were affected.
- **Design notes**: important implementation choices and their rationale.
- **Verification**: commands and tests that were run, with their results.
- **Outstanding work**: what remains unimplemented or unverified.
- **Risks and follow-up**: compatibility, security, performance, or platform considerations.

Do not respond only with "Done." Do not describe planned work as if it has already been implemented.
