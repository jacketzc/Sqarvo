#!/usr/bin/env python3
"""`tools/l10n_gen.py` 的单测（`python3 -m unittest discover -s tools`）。

夹具全部写 tmpdir，不依赖仓库 `l10n/` 也能跑；另有一条「仓库真实 l10n/ 可生成」
的冒烟用例。
"""

from __future__ import annotations

import contextlib
import io
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import l10n_gen  # noqa: E402

REPO_L10N_DIR = l10n_gen.REPO_ROOT / "l10n"

BASE_TOML = """\
[[entry]]
key = "shell.menu.view_language"
zh = "语言"
en = "Language"
status = "done"

[[entry]]
key = "shell.menu.lang_zh"
zh = "简体中文"
en = "简体中文"
status = "done"
"""

PLURAL_TOML = """\
[[entry]]
key = "tabs.status_row_count"
zh = "{n} 行"
en = "{n} rows"
en_one = "1 row"
status = "done"
"""


@contextlib.contextmanager
def quiet():
    """吞掉 stderr 里的告警行（逐条测试自己断言的部分从返回 buffer 读）。"""
    buffer = io.StringIO()
    with contextlib.redirect_stderr(buffer):
        yield buffer


def fixture(tmp: Path, *texts: str) -> Path:
    l10n_dir = tmp / "l10n"
    l10n_dir.mkdir(exist_ok=True)
    for index, text in enumerate(texts):
        (l10n_dir / f"part{index}.toml").write_text(text, encoding="utf-8")
    return l10n_dir


class LoadTest(unittest.TestCase):
    def test_loads_sorted_entries(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(Path(raw), BASE_TOML, PLURAL_TOML)
            entries = l10n_gen.load_entries(l10n_dir)
            self.assertEqual(
                [entry.key for entry in entries],
                [
                    "shell.menu.lang_zh",
                    "shell.menu.view_language",
                    "tabs.status_row_count",
                ],
            )

    def test_duplicate_key_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(Path(raw), BASE_TOML, BASE_TOML)
            with self.assertRaises(l10n_gen.L10nError) as raised:
                l10n_gen.load_entries(l10n_dir)
            self.assertIn("重复 key", str(raised.exception))

    def test_missing_zh_is_error(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(
                Path(raw),
                '[[entry]]\nkey = "shell.menu.x"\nen = "X"\nstatus = "done"\n',
            )
            with self.assertRaises(l10n_gen.L10nError) as raised:
                l10n_gen.load_entries(l10n_dir)
            self.assertIn("缺 zh", str(raised.exception))

    def test_missing_en_is_warning_only(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(
                Path(raw),
                '[[entry]]\nkey = "shell.menu.x"\nzh = "某文案"\nstatus = "pending_en"\n',
            )
            with quiet() as stderr:
                entries = l10n_gen.load_entries(l10n_dir)
            self.assertEqual(stderr.getvalue().strip(), "[l10n] en 缺失：shell.menu.x")
            self.assertIsNone(entries[0].en)

    def test_bad_key_and_unknown_field_are_errors(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(
                Path(raw), '[[entry]]\nkey = "Shell.Menu.X"\nzh = "甲"\nstatus = "done"\n'
            )
            with self.assertRaises(l10n_gen.L10nError):
                l10n_gen.load_entries(l10n_dir)
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(
                Path(raw),
                '[[entry]]\nkey = "shell.menu.x"\nzh = "甲"\nstatus = "done"\nEn = "A"\n',
            )
            with self.assertRaises(l10n_gen.L10nError) as raised:
                l10n_gen.load_entries(l10n_dir)
            self.assertIn("未知字段", str(raised.exception))


class RustGenTest(unittest.TestCase):
    def test_static_match_output(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            entries = l10n_gen.load_entries(fixture(tmp, BASE_TOML, PLURAL_TOML))
            out = tmp / "out" / "l10n_catalog.rs"
            l10n_gen.gen_rust(entries, out)
            text = out.read_text(encoding="utf-8")
            self.assertIn("pub const L10N_KEY_COUNT: usize = 3;", text)
            self.assertIn(
                '(LANG_ZH, "shell.menu.view_language") => Some("语言")', text
            )
            self.assertIn(
                '(LANG_EN, "shell.menu.lang_zh") => Some("简体中文")', text
            )
            # 复数：one 形态只进 lookup_one；other 形态照常进 lookup。
            self.assertIn('(LANG_EN, "tabs.status_row_count") => Some("{n} rows")', text)
            self.assertIn(
                '(LANG_EN, "tabs.status_row_count") => Some("1 row")',
                text.split("pub fn lookup_one")[1],
            )
            self.assertNotIn("1 row", text.split("pub fn lookup_one")[0])


class CliTest(unittest.TestCase):
    def test_main_end_to_end(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            tmp = Path(raw)
            l10n_dir = fixture(tmp, BASE_TOML)
            out = tmp / "cat.rs"
            with quiet():
                self.assertEqual(
                    l10n_gen.main(["--l10n-dir", str(l10n_dir), "rust", "--out", str(out)]),
                    0,
                )
            self.assertIn("pub fn lookup", out.read_text(encoding="utf-8"))

    def test_main_fails_on_duplicate(self) -> None:
        with tempfile.TemporaryDirectory() as raw:
            l10n_dir = fixture(Path(raw), BASE_TOML, BASE_TOML)
            with quiet():
                code = l10n_gen.main(
                    ["--l10n-dir", str(l10n_dir), "rust", "--out", str(Path(raw) / "c.rs")]
                )
            self.assertEqual(code, 1)


class RepoCatalogTest(unittest.TestCase):
    """仓库真实 `l10n/` 必须可生成（S1 首批：语言小节 + 重启确认框）。"""

    def test_repo_l10n_generates(self) -> None:
        self.assertTrue(REPO_L10N_DIR.is_dir(), f"缺目录：{REPO_L10N_DIR}")
        with quiet() as stderr:
            entries = l10n_gen.load_entries(REPO_L10N_DIR)
        self.assertEqual(stderr.getvalue(), "", "仓库首批条目不该有 en 缺失告警")
        keys = {entry.key for entry in entries}
        for expected in [
            "shell.menu.view_language",
            "shell.menu.lang_system",
            "shell.menu.lang_zh",
            "shell.menu.lang_en",
            "shell.dialog_lang_restart_title",
            "shell.dialog_lang_restart_body",
            "shell.dialog_lang_restart_btn_restart",
            "shell.dialog_lang_restart_btn_later",
        ]:
            self.assertIn(expected, keys)
        by_key = {entry.key: entry for entry in entries}
        # zh 必须与 ui.rs 里既有/新增字面量口径逐字节一致（抽样两条）。
        self.assertEqual(by_key["shell.menu.view_language"].zh, "语言")
        self.assertEqual(
            by_key["shell.dialog_lang_restart_body"].zh,
            "重启会关闭所有标签页，未提交的更改会丢失。",
        )
        with tempfile.TemporaryDirectory() as raw:
            out = Path(raw) / "l10n_catalog.rs"
            l10n_gen.gen_rust(entries, out)
            text = out.read_text(encoding="utf-8")
            self.assertIn('(LANG_EN, "shell.menu.lang_en") => Some("English")', text)


if __name__ == "__main__":
    unittest.main()
