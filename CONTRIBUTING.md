# 贡献指南

感谢参与 Sqarvo。提交改动前请完整阅读根目录的 `AGENTS.md`，它是本仓库的技术与安全准则。

## 环境

- Rust 1.88+（后端与 Linux 前端）
- .NET SDK 8+ 与 Windows App SDK（Windows 前端，仅 Windows 可构建）
- Xcode 15+（macOS 前端，仅 macOS 可构建）
- GTK4 / libadwaita 开发包（Linux 前端）

`protoc` 不需要预装：后端构建脚本内置 protoc。

## 任务与 PR

- 具体开发任务在 GitHub Issues 中跟踪；开始前确认所属里程碑（`docs/planning/milestones.md`）
  与需求 ID（`docs/product/requirements.md`）；
- PR 使用仓库模板，关联 Issue 与里程碑，并粘贴真实验证结果；
- `docs/product/` 与 `docs/planning/` 由产品规划/项目管理角色维护；开发提交应更新代码、
  测试、技术文档与 Issue 状态，不擅自修改需求、里程碑或实施计划；
- 规划只维护一份：仓库保存长期规划，GitHub Issues/Projects 保存动态进度。

## 提交前检查

```bash
cd backend
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

无法运行的检查（例如缺少平台的 UI 构建）必须在 PR 中明确说明，不得声称已通过。

## 代码约定

- 修改 `proto/` 后必须重新构建；生成的 gRPC 代码不允许手工编辑，也不提交。
- 不允许把数据库密码写入配置、日志、错误信息、测试夹具或快照。
- 新增能力必须显式声明「支持 / 不支持」，不支持时返回明确错误，禁止静默模拟成功。
- 不要为了增加功能数量而提交没有需求、测试与错误处理的占位实现。
- 大结果集、对象树与文件操作必须有上限、分批或分页策略。
- 不得引入 Electron/WebView/Chromium 作为 UI 运行时。

## 新增数据库驱动

1. 在 `backend/crates/drivers/` 下新增独立 crate（例如 `sqarvo-driver-postgres`），
   实现 `sqarvo-driver-api` 的 `Driver` 与 `DriverSession`；
2. 在 `DriverRegistry::with_builtin_drivers` 中注册；
3. 补充能力描述符：事务、取消、流式结果、分页、Catalog/Schema 层级；
4. 使用专用测试数据库与合成数据编写集成测试，**禁止连接生产数据库**；
5. 更新 `docs/database/driver-spi.md` 与 README 的支持矩阵。

## 提交信息

使用祈使句、说明动机而非仅描述文件改动。示例：

```text
传输层：拒绝非回环 TCP 监听地址

后端在 Windows 上回退为回环 TCP，必须防止误配 0.0.0.0 导致数据库凭据暴露到局域网。
```

## 许可

贡献即表示同意以 Apache License 2.0 授权。
