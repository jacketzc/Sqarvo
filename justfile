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

# ---- UI 功能测试（各端用例库见 tests/ui/README.md；Linux 实现 tests/ui/linux/） ----

# 准备 Linux 端测试 venv（系统 python 的 gi/Atspi + pytest）
ui-test-env:
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    if [ ! -x tests/ui/linux/.venv/bin/python ]; then
        uv venv tests/ui/linux/.venv --python /usr/bin/python3 --system-site-packages
    fi
    uv pip install -q --python tests/ui/linux/.venv/bin/python -r tests/ui/linux/requirements.txt

# 冒烟：启动 / 主窗口可访问性 / 连接对话框 / 关窗断开（本机分派：当前 Linux 唯一实现端）
ui-test-smoke: ui-test-env
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    tests/ui/linux/.venv/bin/python -m pytest tests/ui/linux -c tests/ui/linux/pytest.ini -m smoke

# SQL：SQLite 连接、执行、结果校验、对象树导航
ui-test-sql: ui-test-env
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    tests/ui/linux/.venv/bin/python -m pytest tests/ui/linux -c tests/ui/linux/pytest.ini -m sql

# 全部 UI 功能测试
ui-test-all: ui-test-env
    #!/usr/bin/env bash
    set -euo pipefail
    cd "{{justfile_directory()}}"
    tests/ui/linux/.venv/bin/python -m pytest tests/ui/linux -c tests/ui/linux/pytest.ini

# 显式指定 Linux 端（Windows 端接入后 ui-test-win-* 同构；跨机时在本机调用对应 runner）
ui-test-linux-all: ui-test-all
