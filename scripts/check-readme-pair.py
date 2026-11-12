#!/usr/bin/env python3
"""校验 README.md（英文，仓库首页默认）与 README.zh-CN.md（中文）是同一份交付物。

AGENTS.md §14.1 要求两版同一次提交一起改、结构一致、事实不分歧。人眼很容易漏掉一边，
所以这里做四件机械能判定的事：

1. 两个文件都存在；
2. 各自都还有指向对方的语言切换链接（任何一版删掉切换行都算错）；
3. 相对链接集合相同（措辞可以本地化，链接目标不能一边多一边少），且每个相对目标在仓库里真实存在；
4. 结构规模相同：二级标题数、围栏代码块数、图片数一致（章节顺序与条目数由这些计数兜底）。

不做的事：不比较正文语义、不比较中英文措辞、不猜哪边「更正确」。事实分歧（命令、实测数字）
仍然要人复核，这个脚本只保证两版不会在结构上悄悄分叉。

用法：python3 scripts/check-readme-pair.py   失败返回 1。
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
PAIR = (("README.md", "README.zh-CN.md"), ("README.zh-CN.md", "README.md"))

LINK_RE = re.compile(r"\[[^\]]*\]\(([^)\s]+)\)")
IMAGE_RE = re.compile(r"!\[[^\]]*\]\(([^)\s]+)\)")
# 顶部 logo 与语言切换用原生 HTML（<img src>、<a href>），Markdown 正则抓不到，单独取属性。
HTML_ATTR_RE = re.compile(r'(?:href|src)="([^"]+)"')
H2_RE = re.compile(r"^## ", re.MULTILINE)
FENCE_RE = re.compile(r"^```", re.MULTILINE)


def relative_targets(text: str) -> set[str]:
    """取出相对链接目标（去掉锚点），跳过外链、邮件与纯锚点。Markdown 与 HTML 两种写法都算。"""
    raw = [m.group(1) for m in LINK_RE.finditer(text)]
    raw += [m.group(1) for m in HTML_ATTR_RE.finditer(text)]
    out: set[str] = set()
    for target in raw:
        if target.startswith(("http://", "https://", "mailto:", "#")):
            continue
        out.add(target.split("#", 1)[0])
    return out


def main() -> int:
    errors: list[str] = []
    texts: dict[str, str] = {}

    for name, _ in PAIR:
        path = ROOT / name
        if not path.exists():
            errors.append(f"{name} 不存在：两版必须同时存在（AGENTS.md §14.1）")
            continue
        texts[name] = path.read_text(encoding="utf-8")

    if errors:
        for line in errors:
            print(f"错误：{line}")
        return 1

    # 1. 语言切换链接双向存在
    for name, other in PAIR:
        if other not in relative_targets(texts[name]):
            errors.append(f"{name} 缺少指向 {other} 的语言切换链接")

    # 2. 相对链接集合一致（语言切换链接天然互指对方，先剔除再比较）
    seen = {name: relative_targets(text) for name, text in texts.items()}
    cross = {"README.md", "README.zh-CN.md"}
    cmp = {name: targets - cross for name, targets in seen.items()}
    only_en = cmp["README.md"] - cmp["README.zh-CN.md"]
    only_zh = cmp["README.zh-CN.md"] - cmp["README.md"]
    for target in sorted(only_en):
        errors.append(f"只有 README.md 链接了 {target}，中文版缺少同一入口")
    for target in sorted(only_zh):
        errors.append(f"只有 README.zh-CN.md 链接了 {target}，英文版缺少同一入口")

    # 3. 相对目标必须真实存在
    for name, targets in seen.items():
        for target in sorted(targets):
            if not (ROOT / target).exists():
                errors.append(f"{name} 里的相对链接断链：{target}")

    # 4. 结构规模一致
    structure = {
        name: {
            "二级标题": len(H2_RE.findall(text)),
            "代码块": len(FENCE_RE.findall(text)) // 2,
            "图片": len(IMAGE_RE.findall(text)),
        }
        for name, text in texts.items()
    }
    en, zh = structure["README.md"], structure["README.zh-CN.md"]
    for key in en:
        if en[key] != zh[key]:
            errors.append(f"{key}数量不一致：英文 {en[key]} / 中文 {zh[key]}")

    if errors:
        print(f"README 中英不一致：{len(errors)} 处")
        for line in errors:
            print(f"  - {line}")
        return 1

    parts = " / ".join(f"{key} {en[key]}" for key in en)
    print(f"README 中英一致：{parts}，相对链接 {len(seen['README.md'])} 个全部可达")
    return 0


if __name__ == "__main__":
    sys.exit(main())