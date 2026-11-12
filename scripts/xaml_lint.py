#!/usr/bin/env python3
"""Windows 前端的结构与设计口径门禁。

只依赖 Python 标准库，Linux 与 windows-latest 上行为一致（路径、换行、编码都按
UTF-8 处理，行号按 `\\n` 计）。用法：

    python3 scripts/xaml_lint.py [仓库根目录]

不给参数时以本文件的上一级目录为仓库根，因此从任意工作目录调用都可以。
退出码：有 error → 1，只有 warning 或全干净 → 0。

四条规则（对应 AGENTS.md §8.2、frontend/windows/README.md 与 #170 的崩溃后教训）：

  1. banned-control          禁用控件类型（元素形式 `<X …>` 与 C# `new X(`），命中即 error。
                             这些类型在真机上导致过启动即崩（0xc000027b），一律用原语替代。
  2. font-literal            XAML 里 `FontSize="14"` / `FontWeight="Bold|600"`，命中即 error。
                             字号字重只能取 `{StaticResource Size.*}` / `{StaticResource Weight.*}`。
  3. hardcoded-color         XAML / .cs 里的 `#RRGGBB`，命中即 error。
                             例外只有品牌色板 Tokens.xaml 与 Assets/ 下的资源。
  4. height-without-minheight `Button` / `MenuFlyoutItem` 写了 `Height=` 却没有 `MinHeight=`，
                             warning。WinUI 默认 MinHeight=32 会把高度顶回去（真机踩过）。

输出格式：`<path>:<line>: <error|warning> <规则> <原文>`，末尾一行汇总。
"""

from __future__ import annotations

import argparse
import bisect
import re
import sys
from pathlib import Path
from typing import Iterator, NamedTuple

# ---------------------------------------------------------------- 规则定义

# 大小写敏感：XAML / C# 类型名就是这些拼写，不做大小写折叠，
# 避免把 `togglebutton` 这种根本不存在的类型也算进来（编译器会先报另一条错）。
BANNED_TYPES: tuple[str, ...] = (
    "CommandBar",
    "AppBarButton",
    "ToggleButton",
    "FontIcon",
    "ProgressRing",
    "AutoSuggestBox",
    "InfoBar",
    "DataTemplate",
    "ToggleSwitch",
)

# `Height=` 需要配 `MinHeight=` 的控件类型。
HEIGHT_TYPES: tuple[str, ...] = ("Button", "MenuFlyoutItem")

HEX_COLOR = re.compile(r"#[0-9A-Fa-f]{6}(?![0-9A-Fa-f])")

FONT_SIZE_LITERAL = re.compile(
    r"""FontSize\s*=\s*(?P<q>["'])(?P<value>\s*[-+]?[0-9]+(?:\.[0-9]+)?\s*)(?P=q)"""
)

# 具名字重与数值字重都算字面量；`{StaticResource …}` / `{ThemeResource …}` 不匹配。
FONT_WEIGHT_LITERAL = re.compile(
    r"""FontWeight\s*=\s*(?P<q>["'])(?P<value>\s*(?:
        Thin|ExtraLight|Light|SemiLight|Normal|Medium|SemiBold|Bold|ExtraBold|Black
        |[0-9]{1,4}
    )\s*)(?P=q)""",
    re.VERBOSE,
)

TAG_NAME = re.compile(r"<([A-Za-z_][A-Za-z0-9_.:-]*)")
ATTR_HEIGHT = re.compile(r"(?<![A-Za-z0-9_.:])Height\s*=")
ATTR_MINHEIGHT = re.compile(r"(?<![A-Za-z0-9_.:])MinHeight\s*=")

# `new ToggleButton(…)` / `new Microsoft.UI.Xaml.Controls.ToggleButton { … }`
NEW_BANNED = re.compile(
    r"\bnew\s+(?:[A-Za-z_][A-Za-z0-9_]*\.)*(" + "|".join(BANNED_TYPES) + r")\s*[({]"
)

SKIP_DIRS = {"obj", "bin", ".git", ".vs", "node_modules"}
SCAN_SUFFIXES = {".xaml", ".cs"}


class Hit(NamedTuple):
    path: Path
    line: int
    severity: str
    rule: str
    text: str


class Source:
    """一个待扫描文件：保留原文、行首偏移，用于把 offset 换成行号与原文。"""

    def __init__(self, path: Path, root: Path) -> None:
        self.path = path
        self.rel = path.relative_to(root).as_posix()
        # utf-8-sig 吞掉 BOM；错误字节替换掉而不是崩溃——lint 要能报错，不能自己先死。
        self.text = path.read_text(encoding="utf-8-sig", errors="replace")
        self._line_starts = [0]
        for index, char in enumerate(self.text):
            if char == "\n":
                self._line_starts.append(index + 1)

    def line_of(self, offset: int) -> int:
        return bisect.bisect_right(self._line_starts, offset)

    def line_text(self, offset: int) -> str:
        start = self._line_starts[self.line_of(offset) - 1]
        end = self.text.find("\n", start)
        if end < 0:
            end = len(self.text)
        return self.text[start:end].strip()

    @property
    def is_brand_palette(self) -> bool:
        return self.path.name == "Tokens.xaml"

    @property
    def is_asset(self) -> bool:
        # 任何一层目录叫 Assets（当前是 Sqarvo.Windows/Assets）都算资源目录。
        return "Assets" in self.path.parts


def iter_files(root: Path) -> Iterator[Path]:
    base = root / "frontend" / "windows"
    if not base.is_dir():
        raise SystemExit(f"找不到 {base}：请在仓库根运行，或用参数指定根目录")
    for path in sorted(base.rglob("*")):
        if not path.is_file() or path.suffix.lower() not in SCAN_SUFFIXES:
            continue
        if any(part in SKIP_DIRS for part in path.relative_to(base).parts):
            continue
        yield path


def iter_tags(source: Source) -> Iterator[tuple[str, str, int]]:
    """产出 (元素名, 完整标签文本, 起始 offset)。

    手写扫描而不是一条正则：属性值里可能出现 `>`（例如 `AutomationProperties.Name="A > B"`），
    用 `[^>]*` 会在引号内提前截断，把多行标签切成半个。
    """
    text = source.text
    position = 0
    while True:
        match = TAG_NAME.search(text, position)
        if match is None:
            return
        start = match.start()
        name = match.group(1)
        cursor = match.end()
        quote: str | None = None
        while cursor < len(text):
            char = text[cursor]
            if quote is not None:
                if char == quote:
                    quote = None
            elif char in "\"'":
                quote = char
            elif char == ">":
                break
            elif char == "<":
                # 不是标签（例如注释或文本里的尖括号）：放弃这次，从下一个 `<` 继续。
                break
            cursor += 1
        if cursor < len(text) and text[cursor] == ">":
            yield name, text[start : cursor + 1], start
            position = cursor + 1
        else:
            position = start + 1


def check_xaml(source: Source) -> Iterator[Hit]:
    for name, tag, offset in iter_tags(source):
        if name in BANNED_TYPES:
            yield Hit(source.path, source.line_of(offset), "error", "banned-control", source.line_text(offset))

        height_match = ATTR_HEIGHT.search(tag)
        if name in HEIGHT_TYPES and height_match and not ATTR_MINHEIGHT.search(tag):
            # 报在 `Height=` 那一行：多行标签的开头往往只有 `<Button`，看不出问题在哪。
            height_offset = offset + height_match.start()
            yield Hit(
                source.path,
                source.line_of(height_offset),
                "warning",
                "height-without-minheight",
                source.line_text(height_offset),
            )

    for pattern, rule in ((FONT_SIZE_LITERAL, "font-literal"), (FONT_WEIGHT_LITERAL, "font-literal")):
        for match in pattern.finditer(source.text):
            yield Hit(
                source.path,
                source.line_of(match.start()),
                "error",
                rule,
                source.line_text(match.start()),
            )


def check_colors(source: Source) -> Iterator[Hit]:
    if source.is_brand_palette or source.is_asset:
        return
    for match in HEX_COLOR.finditer(source.text):
        yield Hit(
            source.path,
            source.line_of(match.start()),
            "error",
            "hardcoded-color",
            source.line_text(match.start()),
        )


def check_csharp(source: Source) -> Iterator[Hit]:
    for match in NEW_BANNED.finditer(source.text):
        yield Hit(
            source.path,
            source.line_of(match.start()),
            "error",
            "banned-control",
            source.line_text(match.start()),
        )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Windows 前端 XAML / C# 门禁")
    parser.add_argument(
        "root",
        nargs="?",
        default=str(Path(__file__).resolve().parent.parent),
        help="仓库根目录（默认：本脚本的上一级）",
    )
    args = parser.parse_args(argv)
    root = Path(args.root).resolve()

    # Windows 控制台默认代码页不是 UTF-8（cp1252 / cp936），而本脚本的输出与原文都含中文。
    # 显式切到 UTF-8，否则 windows-latest 上第一步打印就 UnicodeEncodeError。
    for stream in (sys.stdout, sys.stderr):
        reconfigure = getattr(stream, "reconfigure", None)
        if reconfigure is not None:
            reconfigure(encoding="utf-8", errors="replace")

    hits: list[Hit] = []
    scanned = 0
    for path in iter_files(root):
        source = Source(path, root)
        scanned += 1
        if source.path.suffix.lower() == ".xaml":
            # Tokens.xaml 不是页面：它是资源字典，禁用类型的名字会以 `x:Key` 出现，
            # 但标签扫描只认元素名，所以这里不需要额外例外——仍然扫，语义正确。
            hits.extend(check_xaml(source))
            hits.extend(check_colors(source))
        else:
            hits.extend(check_csharp(source))
            hits.extend(check_colors(source))

    hits.sort(key=lambda hit: (hit.path.as_posix(), hit.line, hit.rule))

    for hit in hits:
        print(f"{hit.path.as_posix()}:{hit.line}: {hit.severity} {hit.rule} {hit.text}")

    errors = sum(1 for hit in hits if hit.severity == "error")
    warnings = sum(1 for hit in hits if hit.severity == "warning")
    print(f"汇总：扫描 {scanned} 个文件，error {errors}，warning {warnings}")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())