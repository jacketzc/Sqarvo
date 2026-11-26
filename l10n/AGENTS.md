# l10n/AGENTS.md — 共享文案目录指针

`l10n/` 是共享界面文案的唯一真值。本目录不在 `frontend/` 之下，目录级指令不会自动
继承：修改本目录任何 `*.toml` 之前，必须读 `frontend/AGENTS.md` §8.6（文案规则的权威细则）
与本目录 `l10n/README.md`（条目 schema 与生成链）。

- 运行时不解析本目录文件：构建期由 `tools/l10n_gen.py` 生成 Linux Rust catalog
  （`frontend/linux/build.rs` 调用）；
  生成产物不得手改。
- 区文件名与 UI 注册表 `docs/ui-specification/components/` 的区一一对应，不发明第二套命名。
- 用户可见文案的改动属于 UI 行为变化：按根 §14.3 同 PR 检查注册表对应组件条目并对受影响的 Active 前端执行
  `mark`；失败类文案另受行为契约 §8.4 硬约束。
- 最小验证：`python3 -m unittest discover -s tools`（仓库根执行）；文案结构变更后至少
  构建 `frontend/linux` 一次确认生成通过。
