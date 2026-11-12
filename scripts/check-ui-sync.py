#!/usr/bin/env python3
"""Sqarvo UI 组件注册表的校验 / 生成 / 同步标记工具。

规范说明见 docs/ui-specification/components/README.md，决策见 ADR 0027。

子命令：
    check [--strict]   结构、引用、哈希检查；有硬错误返回 1
    gen                生成 docs/ui-specification/ui-components.md（不要手改产物）
    mark               把某端记录的 spec_hash 刷新为当前规范哈希

设计约束（不要为了「方便」放宽）：
  * 数字只来自 metrics.toml，颜色只来自主题 token —— 组件里出现 px 或 #RRGGBB 是缺陷；
  * 同步标记是派生的：规范哈希变了就自动过期，不依赖任何人记得清理；
  * impl_notes / platform_notes 不参与哈希（各端实现细节不该把别的端打回）。
"""

from __future__ import annotations

import hashlib
import json
import re
import sys
import tomllib
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
COMP_DIR = REPO / "docs/ui-specification/components"
METRICS_PATH = REPO / "docs/ui-specification/metrics.toml"
THEME_PATH = REPO / "frontend/linux/src/theme.rs"
OUT_MD = REPO / "docs/ui-specification/ui-components.md"

PLATFORMS = ("linux", "windows", "macos")
PLATFORM_LABEL = {"linux": "Linux", "windows": "Windows", "macos": "macOS"}

KINDS = {"area", "container", "control", "row", "cell", "item", "slot"}
STATUSES = {"active", "deprecated", "removed"}
TRIGGERS = {
    "click",
    "double_click",
    "right_click",
    "hover",
    "focus",
    "key",
    "drag",
    "scroll",
    "load",
}
STATE_KEYS = {"hover", "active", "selected", "focus", "disabled"}
SYNC_STATES = {"done", "partial", "none", "na", "blocked"}

# P0 暂定槽位；workspace-model 发布正式槽位 id 后改为读它。
SLOTS = {
    "header",
    "menubar",
    "toolbar",
    "sidebar",
    "workspace.content",
    "workspace.tabstrip",
    "inspector",
    "status_bar.left_gutter",
    "status_bar.row1",
    "status_bar.row2",
    "dialogs",
    "startup",
    "overlay",
}

# 不参与规范哈希：同步状态本身、各端实现细节与平台差异。
HASH_EXCLUDE = {"sync", "impl_notes", "platform_notes"}

ID_RE = re.compile(r"^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)*$")
REF_RE = re.compile(r"((?:frontend/)?[\w./-]+\.(?:rs|xaml|cs|swift)):(\d+)(?:-(\d+))?")
REF_BARE_RE = re.compile(r"((?:frontend/)?[\w./-]+\.(?:rs|xaml|cs|swift))")
TOKEN_RE = re.compile(r"@([A-Za-z0-9_]+)")
HEX_RE = re.compile(r"#[0-9A-Fa-f]{6}\b")
NATIVE_RE = re.compile(r"^gtk:.+ / winui:.+ / swift:.+$")


class Report:
    def __init__(self, strict: bool) -> None:
        self.errors: list[str] = []
        self.warnings: list[str] = []
        self.stale: list[str] = []
        self.strict = strict

    def error(self, where: str, msg: str) -> None:
        self.errors.append(f"{where}: {msg}")

    def warn(self, where: str, msg: str) -> None:
        self.warnings.append(f"{where}: {msg}")

    def ok(self) -> bool:
        if self.errors:
            return False
        if self.strict and (self.stale or self.warnings):
            return False
        return True


# ---------------------------------------------------------------- 数据装载


def load_metrics_keys() -> set[str]:
    data = tomllib.loads(METRICS_PATH.read_text(encoding="utf-8"))
    tiers = data.get("tier", {})
    keys: set[str] = set()
    for tier in tiers.values():
        keys.update(k for k in tier if k not in {"id", "label", "scale"})
    return keys


def load_theme_tokens() -> set[str]:
    text = THEME_PATH.read_text(encoding="utf-8")
    return set(TOKEN_RE.findall(text))


def load_areas() -> list[dict]:
    areas = []
    for path in sorted(COMP_DIR.glob("*.toml")):
        data = tomllib.loads(path.read_text(encoding="utf-8"))
        if "area" not in data:
            raise SystemExit(f"{path.name}: 缺少 [area] 段")
        areas.append({"path": path, "area": data["area"], "components": data.get("component", [])})
    if not areas:
        raise SystemExit(f"{COMP_DIR} 下没有任何 *.toml")
    return areas


def canonical_hash(comp: dict) -> str:
    payload = {k: v for k, v in comp.items() if k not in HASH_EXCLUDE}
    blob = json.dumps(payload, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(blob.encode("utf-8")).hexdigest()[:12]


# ---------------------------------------------------------------- 校验


def check_style(where: str, comp: dict, metrics: set[str], tokens: set[str], rep: Report) -> None:
    style = comp.get("style")
    if style is None:
        return
    slot = style.get("slot")
    if slot is not None and slot not in SLOTS:
        rep.error(where, f"style.slot = {slot!r} 不在暂定槽位清单里（见 components/README.md）")
    for key in style.get("metrics", []):
        if key not in metrics:
            rep.error(where, f"style.metrics 里的 {key!r} 不是 metrics.toml 的 key")
    for tok in style.get("tokens", []):
        if tok not in tokens:
            rep.error(where, f"style.tokens 里的 {tok!r} 在 theme.rs 里找不到 @{tok}")
    for state, tok in style.get("states", {}).items():
        if state not in STATE_KEYS:
            rep.error(where, f"style.states 的键 {state!r} 不在 {sorted(STATE_KEYS)}")
        if tok not in tokens:
            rep.error(where, f"style.states.{state} = {tok!r} 在 theme.rs 里找不到 @{tok}")
    for field in ("slot",):
        value = style.get(field)
        if isinstance(value, str) and HEX_RE.search(value):
            rep.error(where, f"style.{field} 里出现色值字面量：{value}")
    blob = json.dumps(style, ensure_ascii=False)
    if HEX_RE.search(blob):
        rep.error(where, "style 里出现 #RRGGBB 字面量：颜色只能引用主题 token")


def check_source(where: str, source: str, rep: Report) -> None:
    if not source:
        rep.error(where, "interaction.source 为空：必须给源码依据")
        return
    refs = REF_RE.findall(source)
    if not refs:
        bare = REF_BARE_RE.findall(source)
        if not bare:
            rep.error(where, f"interaction.source 里没有可识别的源码引用：{source!r}")
            return
        for path in bare:
            resolve = REPO / path if path.startswith("frontend/") else REPO / "frontend/linux" / path
            if not resolve.is_file():
                rep.error(where, f"源码引用不存在：{path}")
        return
    for path, start, end in refs:
        resolve = REPO / path if path.startswith("frontend/") else REPO / "frontend/linux" / path
        if not resolve.is_file():
            rep.error(where, f"源码引用不存在：{path}")
            continue
        total = len(resolve.read_text(encoding="utf-8").splitlines())
        for raw in (start, end):
            if raw and int(raw) > total:
                rep.error(where, f"{path}:{raw} 超出文件行数（{total}）")


def check_component(
    where: str,
    comp: dict,
    ids: set[str],
    metrics: set[str],
    tokens: set[str],
    rep: Report,
) -> None:
    cid = comp.get("id", "")
    if not cid:
        rep.error(where, "缺少 id")
        return
    if not ID_RE.match(cid):
        rep.error(where, f"id 不合法：{cid!r}（小写 ASCII、点号分层）")
    if "display_name" not in comp:
        rep.error(where, "缺少 display_name")
    if comp.get("kind") not in KINDS:
        rep.error(where, f"kind = {comp.get('kind')!r} 不在 {sorted(KINDS)}")
    status = comp.get("status")
    if status not in STATUSES:
        rep.error(where, f"status = {status!r} 不在 {sorted(STATUSES)}")
    if status == "deprecated":
        if not comp.get("superseded_by"):
            rep.error(where, "status=deprecated 必须给 superseded_by")
        elif comp["superseded_by"] not in ids:
            rep.error(where, f"superseded_by = {comp['superseded_by']!r} 不存在")

    parent = comp.get("parent", "")
    kind = comp.get("kind")
    if kind == "area":
        if parent:
            rep.error(where, "区域根（kind=area）的 parent 必须为空")
    else:
        if not parent:
            rep.error(where, f"非区域根缺少 parent（id={cid}）")
        elif parent not in ids:
            rep.error(where, f"parent = {parent!r} 不存在")
        elif not cid.startswith(parent + "."):
            rep.error(where, f"id 与 parent 层级不一致：{cid} 应以 {parent}. 开头")

    check_style(where, comp, metrics, tokens, rep)

    interactions = comp.get("interaction", [])
    for i, it in enumerate(interactions):
        iw = f"{where}#interaction[{i}]"
        trig = it.get("trigger")
        if trig not in TRIGGERS:
            rep.error(iw, f"trigger = {trig!r} 不在 {sorted(TRIGGERS)}")
        if trig == "key" and not it.get("keys"):
            rep.error(iw, "trigger=key 必须给 keys（如 [\"Ctrl+F\"]）")
        if not it.get("result"):
            rep.error(iw, "缺少 result")
        check_source(iw, it.get("source", ""), rep)

    a11y = comp.get("a11y", {})
    if kind in {"control", "row", "item"} and interactions and not a11y.get("name"):
        rep.warn(where, "有交互但缺 a11y.name（P1 补全）")

    hand = comp.get("hand_drawn", False)
    why = comp.get("why_not_native")
    if hand and not why:
        rep.error(where, "hand_drawn=true 必须给 why_not_native（自绘要有理由）")
    if why is not None and not isinstance(why, str):
        rep.error(where, "why_not_native 必须是字符串")
    native = comp.get("native_control")
    if native is not None and not NATIVE_RE.match(native):
        rep.error(
            where,
            f"native_control 应形如 `gtk:<控件> / winui:<控件> / swift:<控件>`，现为 {native!r}",
        )

    sync = comp.get("sync", {})
    for plat in PLATFORMS:
        entry = sync.get(plat)
        if not entry:
            rep.error(where, f"缺少 sync.{plat}")
            continue
        state = entry.get("state")
        if state not in SYNC_STATES:
            rep.error(where, f"sync.{plat}.state = {state!r} 不在 {sorted(SYNC_STATES)}")
            continue
        recorded = entry.get("spec_hash", "")
        current = canonical_hash(comp)
        if state in {"done", "partial"}:
            if not recorded:
                rep.error(where, f"sync.{plat}.state={state} 必须带 spec_hash（用 mark 生成）")
            elif recorded != current:
                rep.stale.append(f"{cid} [{plat}] 标记 {recorded} ≠ 当前规范 {current}")
        elif recorded:
            rep.error(where, f"sync.{plat}.state={state} 时 spec_hash 必须为空（没核对过就别记哈希）")
        if state == "done" and not entry.get("evidence"):
            rep.warn(where, f"sync.{plat}=done 但没有 evidence")
        note = entry.get("note") or comp.get("platform_notes", {}).get(plat)
        if state in {"partial", "na", "blocked"} and not note:
            rep.warn(where, f"sync.{plat}={state} 但没有说明（note / platform_notes.{plat}）")


def run_check(strict: bool) -> int:
    metrics = load_metrics_keys()
    tokens = load_theme_tokens()
    areas = load_areas()
    rep = Report(strict)

    all_ids: dict[str, str] = {}
    for item in areas:
        path = item["path"]
        area = item["area"]
        aid = area.get("id", "")
        if aid != path.stem:
            rep.error(path.name, f"[area].id = {aid!r} 与文件名 {path.stem!r} 不一致")
        if not area.get("display_name"):
            rep.error(path.name, "[area] 缺少 display_name")
        for comp in item["components"]:
            cid = comp.get("id", "")
            if cid in all_ids:
                rep.error(path.name, f"id 重复：{cid}（也在 {all_ids[cid]}）")
            else:
                all_ids[cid] = path.name
        if aid and aid not in {c.get("id") for c in item["components"]}:
            rep.error(path.name, f"区域根组件 {aid!r} 没有定义（必须有 [[component]] kind=area）")

    for item in areas:
        path = item["path"]
        for comp in item["components"]:
            check_component(
                f"{path.name}#{comp.get('id', '?')}", comp, set(all_ids), metrics, tokens, rep
            )

    total = len(all_ids)
    print(f"组件总数：{total}（{len(areas)} 个功能区）")
    for item in areas:
        area = item["area"]
        comps = item["components"]
        line = [f"  {area.get('display_name', '?')}（{area['id']}）：{len(comps)} 个"]
        for plat in PLATFORMS:
            counts: dict[str, int] = {}
            for comp in comps:
                state = comp.get("sync", {}).get(plat, {}).get("state", "?")
                counts[state] = counts.get(state, 0) + 1
            parts = "、".join(f"{k} {v}" for k, v in sorted(counts.items()))
            line.append(f"{PLATFORM_LABEL[plat]} {parts}")
        print(" · ".join(line))

    if rep.stale:
        print(f"\n同步标记已过期（规范改了但该端没重新核对）：{len(rep.stale)} 条")
        for line in rep.stale:
            print(f"  ! {line}")
    if rep.warnings:
        print(f"\n警告：{len(rep.warnings)} 条")
        for line in rep.warnings:
            print(f"  ~ {line}")
    if rep.errors:
        print(f"\n错误：{len(rep.errors)} 条")
        for line in rep.errors:
            print(f"  x {line}")
        return 1
    if strict and (rep.stale or rep.warnings):
        print("\n--strict：过期 / 警告按失败处理")
        return 1
    print("\n结构检查通过。")
    return 0


# ---------------------------------------------------------------- 生成文档


def join_notes(items: list[str]) -> str:
    """按中文标点拼接多条备注：前一条已以句号 / 分号结尾就不再加分隔符。"""
    out = ""
    for i, item in enumerate(items):
        if i and not out.endswith(("。", "；", "：", "）")):
            out += "；"
        out += item
    return out


def sync_cell(comp: dict, plat: str) -> str:
    entry = comp.get("sync", {}).get(plat, {})
    state = entry.get("state", "?")
    mark = {"done": "done", "partial": "partial", "none": "—", "na": "na", "blocked": "blocked"}
    if state in mark:
        return mark[state]
    return str(state)


def run_gen() -> int:
    areas = load_areas()
    out: list[str] = []
    out.append("# UI 组件总览")
    out.append("")
    out.append("> 本文件由 `scripts/check-ui-sync.sh gen` 生成，源是")
    out.append("> [`components/*.toml`](components/)，**不要手改**。")
    out.append("> 字段与规则见 [`components/README.md`](components/README.md)、")
    out.append("> [ADR 0027](../decisions/0027-ui-component-registry.md)。")
    out.append("")

    total = sum(len(a["components"]) for a in areas)
    out.append(f"共 {total} 个组件 / {len(areas)} 个功能区。同步列：`done` 已实现并按本规范核对过，")
    out.append("`partial` 部分实现（差异见组件明细的「平台」），`—` 未实现，`na` 该平台不做，")
    out.append("`blocked` 平台能力做不到。")
    out.append("")

    for item in areas:
        area = item["area"]
        out.append(f"## {area.get('display_name', area['id'])}（`{area['id']}`）")
        out.append("")
        if area.get("summary"):
            out.append(area["summary"])
            out.append("")
        out.append("| 组件 | 名称 | kind | 状态 | Linux | Windows | macOS |")
        out.append("|---|---|---|---|---|---|---|")
        for comp in item["components"]:
            indent = comp["id"].count(".")
            name = "　" * indent + comp.get("display_name", "")
            out.append(
                f"| `{comp['id']}` | {name} | {comp.get('kind', '')} | {comp.get('status', '')} "
                f"| {sync_cell(comp, 'linux')} | {sync_cell(comp, 'windows')} "
                f"| {sync_cell(comp, 'macos')} |"
            )
        out.append("")

        for comp in item["components"]:
            out.append(f"### `{comp['id']}` {comp.get('display_name', '')}")
            out.append("")
            meta = [f"kind `{comp.get('kind', '')}`", f"status `{comp.get('status', '')}`"]
            if comp.get("parent"):
                meta.append(f"parent `{comp['parent']}`")
            if comp.get("superseded_by"):
                meta.append(f"→ `{comp['superseded_by']}`")
            out.append(" · ".join(meta))
            out.append("")
            style = comp.get("style", {})
            if style:
                bits = []
                if style.get("slot"):
                    bits.append(f"槽位 `{style['slot']}`")
                if style.get("metrics"):
                    bits.append("尺寸 " + "、".join(f"`{m}`" for m in style["metrics"]))
                if style.get("tokens"):
                    bits.append("颜色 " + "、".join(f"`{t}`" for t in style["tokens"]))
                if style.get("states"):
                    bits.append(
                        "状态 "
                        + "、".join(f"{k}=`{v}`" for k, v in style["states"].items())
                    )
                if bits:
                    out.append("- 样式：" + "；".join(bits))
            if comp.get("a11y"):
                a11y = comp["a11y"]
                bits = []
                if a11y.get("name"):
                    bits.append(f"可访问名「{a11y['name']}」")
                if a11y.get("tab"):
                    bits.append(f"Tab {a11y['tab']}")
                if bits:
                    out.append("- 无障碍：" + "；".join(bits))
            if comp.get("native_control"):
                out.append("- 原生控件：" + comp["native_control"])
            if comp.get("hand_drawn"):
                out.append("- 自绘：" + comp.get("why_not_native", ""))
            elif comp.get("why_not_native"):
                out.append("- 原生选型说明：" + comp["why_not_native"])
            notes = comp.get("platform_notes", {})
            if notes:
                out.append(
                    "- 平台差异：" + "；".join(f"{PLATFORM_LABEL.get(k, k)}：{v}" for k, v in notes.items())
                )
            sync = comp.get("sync", {})
            if sync:
                parts = []
                for plat in PLATFORMS:
                    entry = sync.get(plat, {})
                    state = entry.get("state", "?")
                    detail = entry.get("note") or entry.get("evidence") or ""
                    parts.append(
                        f"{PLATFORM_LABEL[plat]} {state}" + (f"（{detail}）" if detail else "")
                    )
                out.append("- 同步：" + "；".join(parts))
            impl = comp.get("impl_notes", [])
            if impl:
                out.append("- 实现备注：" + join_notes(impl))
            interactions = comp.get("interaction", [])
            if interactions:
                out.append("")
                out.append("| 触发 | 前置状态与可用性 | 结果（含文案/错误/取消） | 源码依据 |")
                out.append("|---|---|---|---|")
                for it in interactions:
                    trig = it.get("trigger", "")
                    if it.get("keys"):
                        trig += " " + " / ".join(it["keys"])
                    out.append(
                        f"| {trig} | {it.get('precondition', '—')} | {it.get('result', '')} "
                        f"| `{it.get('source', '')}` |"
                    )
            out.append("")

    OUT_MD.write_text("\n".join(out).rstrip() + "\n", encoding="utf-8")
    print(f"已生成 {OUT_MD.relative_to(REPO)}（{total} 个组件）")
    return 0


# ---------------------------------------------------------------- 同步标记


def run_mark(cid: str | None, plat: str, all_mode: bool) -> int:
    if plat not in PLATFORMS:
        raise SystemExit(f"未知平台：{plat}")
    areas = load_areas()
    touched = 0
    for item in areas:
        path: Path = item["path"]
        text = path.read_text(encoding="utf-8")
        lines = text.splitlines()
        starts = [i for i, ln in enumerate(lines) if ln.strip() == "[[component]]"]
        if not starts:
            continue
        starts.append(len(lines))
        changed = False
        # 从后往前改，避免索引漂移。
        for block_start, block_end in reversed(list(zip(starts, starts[1:]))):
            block = lines[block_start:block_end]
            id_line = next((ln for ln in block if ln.startswith("id = ")), "")
            if not id_line:
                continue
            this_id = id_line.split("=", 1)[1].strip().strip('"')
            if not all_mode and this_id != cid:
                continue
            comp = next((c for c in item["components"] if c.get("id") == this_id), None)
            if comp is None:
                continue
            state = comp.get("sync", {}).get(plat, {}).get("state", "none")
            if state in {"none", "na", "blocked"}:
                if not all_mode:
                    print(f"{this_id}: state={state}，不记哈希（先改 state 再 mark）")
                continue
            new_hash = canonical_hash(comp)
            header = f"[component.sync.{plat}]"
            try:
                h = block.index(header)
            except ValueError:
                print(f"{this_id}: 缺少 {header}，跳过（先补块）")
                continue
            target = None
            for j in range(h + 1, len(block)):
                if block[j].startswith("["):
                    break
                if block[j].startswith("spec_hash"):
                    target = j
                    break
            if target is None:
                print(f"{this_id}: {header} 里没有 spec_hash 行，跳过")
                continue
            block[target] = f'spec_hash = "{new_hash}"'
            lines[block_start:block_end] = block
            changed = True
            touched += 1
        if changed:
            path.write_text("\n".join(lines) + "\n", encoding="utf-8")
    if touched == 0:
        print("没有可更新的标记（检查 id / platform / state）")
        return 1
    print(f"已更新 {touched} 条 spec_hash")
    return 0


# ---------------------------------------------------------------- 入口


def main(argv: list[str]) -> int:
    cmd = argv[0] if argv else "check"
    args = argv[1:]
    if cmd in {"-h", "--help", "help"}:
        print(__doc__)
        return 0
    if cmd == "check":
        return run_check(strict="--strict" in args)
    if cmd == "gen":
        return run_gen()
    if cmd == "mark":
        plat = "linux"
        cid = None
        all_mode = "--all" in args
        for i, a in enumerate(args):
            if a == "--platform" and i + 1 < len(args):
                plat = args[i + 1]
            if a == "--id" and i + 1 < len(args):
                cid = args[i + 1]
        if not all_mode and not cid:
            print("mark 需要 --id <组件 id> 或 --all")
            return 1
        return run_mark(cid, plat, all_mode)
    print(f"未知子命令：{cmd}")
    print(__doc__)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))