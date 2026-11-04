# Sqarvo 常用开发任务。使用 `just`（https://github.com/casey/just）。

default:
    @just --list

# 格式化后端代码
fmt:
    cd backend && cargo fmt --all

# 检查格式（不修改文件）
fmt-check:
    cd backend && cargo fmt --all -- --check

# Clippy（警告视为错误）
lint:
    cd backend && cargo clippy --all-targets -- -D warnings

# 运行全部测试（含真实 UDS 的 gRPC 冒烟测试）
test:
    cd backend && cargo test

# 提交前检查
check: fmt-check lint test

# 启动后端（Unix socket 位于用户运行时目录）
run:
    cd backend && cargo run -p sqarvo-backend-service

# 启动后端并输出调试日志
run-debug:
    cd backend && cargo run -p sqarvo-backend-service -- --log-filter debug

# 生成文档
doc:
    cd backend && cargo doc --no-deps --workspace
