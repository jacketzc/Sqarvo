# UI 功能测试用例库（三端）

用户可见行为（交互、状态流转、可访问性）的验证以**该平台自动化功能用例**为载体
（根 AGENTS §13、frontend/AGENTS §13）：改哪个端的行为，先保证该端有覆盖该行为的
用例，缺失则先补用例、再改再验；只断言交互与功能，不做视觉断言（ADR 0029）。

## 布局

```
tests/ui/
  README.md      ← 本文件：三端共同规范（唯一真值）
  linux/         ← 已实现：Python + pytest + AT-SPI（GTK4 a11y），详见 linux/README.md
  windows/       ← 待建：C#（UIA，如 FlaUI）+ 同三层结构，接入时以 linux/ 为参照
  macos/         ← 待建：XCUITest 同构
```

各端**共享的是约定，不是代码**：平台自动化栈完全不同，不建跨端测试框架抽象；
各端目录内自足（依赖、venv/构建产物、工作区、诊断输出），互不引用。

每端内部三层结构（命名可随平台语言习惯）：

| 层 | 职责 |
|---|---|
| 适配层 | 按语义（role + 可访问名/automation id）定位控件；点击/输入/状态读取；显式等待 |
| 复用操作 | 本应用的常见流程封装（连接、执行 SQL、树导航……） |
| 场景 | 组合操作 + 断言；用例 docstring 标注注册表组件 `id` |

## 统一契约（三端都必须一致）

1. **选择器只用语义**（可访问角色 + 名称/automation id），禁止坐标点击、截图识别、
   OCR 或视觉模型定位；名称对齐 `docs/ui-specification/components/*.toml` 的
   `a11y.name`，不发明第二套。
2. **输出**：正常执行只打摘要——

   ```
   UI TEST: PASS | FAIL
   Passed: n
   Failed: n
   [Skipped: n]
   Duration: x.xs
   ```

   失败逐条给 `Test / Step / Expected / Actual / Error / Diagnostic`；
   完整控件树 dump 等详细诊断**只在失败时**落盘到该端 `logs/`，不进常规输出。
3. **机器可读报告**：该端 `results/report.json`（每条用例 outcome/duration/step/
   expected/actual）。
4. **退出码**：0 全过；1 有失败；3 环境不可行（缺依赖/缺二进制/残留实例）——
   环境不可行按根 §13 三态报告为 **BLOCKED + 原因**，不得伪造通过。
5. **隔离**：每实例独立配置/数据目录（Linux `XDG_*_HOME`、Windows 临时
   `%LOCALAPPDATA%`、macOS 临时容器目录）+ 独立测试数据库 + 进程看门狗超时 +
   退出清理断言（无残留进程、无用户配置写入）；绝不触碰用户真实配置、凭据库、
   生产数据库。
6. **确定性**：显式条件等待 + 超时，无裸 sleep；用例相互独立；同一环境可重复运行。
7. **已知限制挂账**：平台环境做不到的事（如某端禁止输入合成）写进该端 README 的
   「已知限制」，对应用例显式 skip + 原因，不得静默绕过或弱化断言。
8. **截图**：默认关闭；仅当任务验收条件明确声明时才允许，且不作为元素定位或断言
   手段，不触发任何视觉分析。

## 组件覆盖反查（全局）

`grep -rl <组件id> tests/ui/` —— 任何端命中即视为该组件有功能用例；注册表
`sync.<端>.evidence` 直接指向该端用例路径与断言。新增/变更用户可见组件的同一 PR
必须补/改用例（frontend/AGENTS §13）。

## 命令命名空间（justfile）

- `just ui-test-env` / `ui-test-smoke` / `ui-test-sql` / `ui-test-all`：
  **本机分派**——在当前平台跑当前平台的用例库（Linux 已接入；Windows 接入后同义）。
- `just ui-test-linux-all`（已实现）与未来的 `ui-test-win-*`：**显式指定端**，
  用于 CI 矩阵或跨机调度场景。
