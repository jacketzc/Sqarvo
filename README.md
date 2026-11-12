<p align="center">
  <b>English</b> · <a href="README.zh-CN.md">简体中文</a>
</p>

<p align="center">
  <img src="docs/assets/brand/sqarvo-logo.svg" width="112" height="112" alt="Sqarvo logo">
</p>

<h1 align="center">Sqarvo</h1>

<p align="center">An open-source, lightweight, native desktop database client.</p>

## Why Sqarvo

- **Natively built** — WinUI 3 · GTK4 · SwiftUI, each platform using its own controls and interaction
  conventions; no Chromium / WebView, no JavaScript.
- **Low footprint** — about 70 MB idle.
- **Minimal UI** — only what daily work needs: connect, write SQL, browse data.

![Sqarvo main window: object tree, table data grid and inspector DDL (Linux / GTK4)](docs/assets/screenshots/linux-main-window.png)

## Status

The Linux client works end to end (SQLite and MariaDB); Windows and macOS are window shells that do
not talk to the backend yet.

- **Available**: connections and connection tests, passwords stored only in the OS credential store,
  object tree and metadata (lazy per level, search by name), SQL editor with paged results, table
  browsing / editing / inserting / explicit transactions, export to CSV / INSERT / table dump,
  read-only DDL view.
- **Missing**: PostgreSQL driver, multi-statement execution, DDL changes, import, schema compare and
  ER diagrams, Windows / macOS integration, remote TLS. Unsupported capabilities return an explicit
  `UNIMPLEMENTED` / `capability.unsupported` instead of pretending to succeed.
- Capability lists and the support matrix live in
  [requirements](docs/product/requirements.md) and the
  [release plan](docs/planning/release-plan.md); per-version notes are in
  [`docs/releases/`](docs/releases/).

## Repository layout

```text
proto/                 Protobuf contracts (single source of truth)
backend/               Rust workspace: driver SPI, drivers (SQLite / MariaDB),
                       query and transactions, metadata, credentials, local
                       transport, gRPC services
frontend/
  linux/               GTK4 + libadwaita + Rust (wired to the backend)
  windows/             WinUI 3 + C# (window shell)
  macos/               SwiftUI + Swift (window shell)
docs/                  Product, planning, architecture, API, drivers, UI spec, ADRs
tests/                 Contract / integration tests and fixtures
packaging/             Per-platform packaging
```

Layer boundaries, and what may never appear in each layer, are in [AGENTS.md](AGENTS.md).

## Build and run

Requires Rust 1.88+; `protoc` is **not** needed (bundled with the build script).

```bash
cd backend
cargo build && cargo test
cargo clippy --all-targets && cargo fmt --check
```

Run the backend (it writes an endpoint file with a token, mode 0600, and removes it on a clean exit):

```bash
cargo run -p sqarvo-backend-service
# --socket /tmp/sqarvo.sock   --listen 127.0.0.1:0 (debugging)   --log-filter debug
```

Run the Linux frontend (GTK 4.16 / libadwaita 1.6+; it starts the backend and performs the handshake):

```bash
sudo apt install libgtk-4-dev libadwaita-1-dev build-essential pkg-config
cd frontend/linux && cargo run
```

MariaDB integration tests need a real database (they skip with a printed reason when the variable is
unset):

```bash
cd backend
SQARVO_MARIADB_URL='mysql://sqarvo_test:***@127.0.0.1:53306/sqarvo_test' \
  cargo test -p sqarvo-driver-mariadb --test integration
```

Starting the test database: [tests/integration/README.md](tests/integration/README.md).

> Built and tested on Linux only. The Windows and macOS transport and UI code has not been verified
> on this machine.

## Documentation

- Product: [vision](docs/product/vision.md) · [positioning](docs/product/positioning.md) ·
  [requirements](docs/product/requirements.md) · [roadmap](docs/product/roadmap.md)
- Planning: [development plan](docs/planning/development-plan.md) ·
  [milestones](docs/planning/milestones.md) · [release plan](docs/planning/release-plan.md) ·
  [risks](docs/planning/risk-register.md)
- Technical: [architecture](docs/architecture/overview.md) ·
  [local IPC](docs/architecture/local-ipc.md) · [gRPC conventions](docs/api/grpc-conventions.md) ·
  [driver SPI](docs/database/driver-spi.md)
- UI: [design principles](docs/ui-specification/design-principles.md) ·
  [areas and layout](docs/ui-specification/workspace-model.md) ·
  [behavior contract](docs/ui-specification/ui-behavior-contract.md) ·
  [component registry](docs/ui-specification/components/README.md) ·
  [scaling and window sizing](docs/ui-specification/scaling-and-window-sizing.md)
- Decisions: [ADRs](docs/decisions/)

## Tasks and progress

Board: [Sqarvo roadmap](https://github.com/users/jacketzc/projects/2) ·
Issues: [jacketzc/Sqarvo](https://github.com/jacketzc/Sqarvo/issues).
Long-term planning lives in `docs/` with the code; GitHub holds only the live execution state.

## Security

A database client handles highly sensitive credentials and production data. Please never put real
passwords, connection strings, schemas or rows into issues, pull requests, logs or test snapshots.
How to report a vulnerability: [SECURITY.md](SECURITY.md).

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Formatting, Clippy and tests must pass before you push;
every task links an issue and a milestone, and pull requests use the repository template.

## License

[Apache License 2.0](LICENSE). This is an independent implementation, not affiliated with Navicat or
any commercial product, and it uses none of their code, icons, wording or UI assets.
