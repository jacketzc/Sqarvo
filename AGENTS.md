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
├── docs/
│   ├── product/              # Product positioning, requirements, roadmap
│   ├── architecture/         # Overall architecture, process model, data flow
│   ├── api/                  # gRPC / Protobuf design notes
│   ├── database/             # Driver SPI, dialects, and database differences
│   ├── ui/                   # Interaction specifications, UI flows, design principles
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

## 15. Agent Workflow

Follow this sequence when executing a task:

1. Read this file and any other `AGENTS.md` files in the directories involved in the task.
2. Read the relevant product requirements, architecture documents, ADRs, and existing code. Do not guess the state of the project based only on the task description.
3. Confirm the requirements, scope of impact, module boundaries, and acceptance criteria. For complex tasks, provide a brief implementation plan first.
4. Prefer the smallest complete and verifiable change. Do not opportunistically rewrite unrelated modules.
5. Before changing APIs or public types, inspect all callers and compatibility implications.
6. Add or update tests, documentation, and examples.
7. Run relevant formatters, static checks, unit tests, and integration tests.
8. Review `git diff` to ensure it contains no secrets, personal data, generated artifacts, temporary files, or unrelated formatting changes.
9. Report completed work, key design decisions, test results, unfinished items, and known risks.

### Agent Prohibitions

- Never fabricate command output, test results, database connection results, or API behavior.
- Do not rebuild existing modules without first reading the existing code and documentation.
- Do not modify user data, production databases, system services, network configuration, or credentials without authorization.
- Do not delete unknown files, overwrite user work, or perform destructive database operations.
- Never commit real passwords, tokens, private keys, certificates, or sensitive connection strings.
- Never edit generated Protobuf/gRPC files manually.
- Never bypass the Driver SPI by hardcoding a database implementation in the frontend or generic business layer.
- Never introduce Electron, Chromium, or WebView to achieve superficial cross-platform support.
- Never describe unimplemented, untested, or UI-placeholder functionality as completed.
- Do not introduce a new runtime, framework, or large dependency without a clear requirement.

## 16. Task Handoff Template

When completing a development task, the final report must include at least:

- **Completed**: what was implemented and which modules were affected.
- **Design notes**: important implementation choices and their rationale.
- **Verification**: commands and tests that were run, with their results.
- **Outstanding work**: what remains unimplemented or unverified.
- **Risks and follow-up**: compatibility, security, performance, or platform considerations.

Do not respond only with "Done." Do not describe planned work as if it has already been implemented.
