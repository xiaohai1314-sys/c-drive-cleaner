"""P0~P2 修改方案的回归测试：硬名单、注册表备份门、规则 v2、残留比对。

与 test_safety 一样是纯逻辑测试，在 Linux 上用临时目录与假对象运行。
"""
import json
import os
import sys
import tempfile
import time
import unittest
from unittest import mock

import categories
import cleaner
import leftover
import registry
import ruleupdate
import safepath
import scanner


class NeverCleanTests(unittest.TestCase):
    def test_recovery_and_restore_points_blocked(self):
        self.assertTrue(safepath.is_never_clean(r"C:\Recovery\Customizations\apps.ppkg"))
        self.assertTrue(safepath.is_never_clean(r"D:\System Volume Information\restore.gpt"))
        self.assertTrue(safepath.is_never_clean(r"C:\Windows\Temp\usmt.ppkg"))

    def test_normal_paths_allowed(self):
        self.assertFalse(safepath.is_never_clean(r"C:\Windows\Temp\junk.tmp"))
        self.assertFalse(safepath.is_never_clean(r"C:\$Recycle.Bin\S-1-5-21-x\$RABC"))

    def test_execution_forbidden_covers_trusted_rules(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "usmt.ppkg")
            with open(target, "w") as handle:
                handle.write("x")
            self.assertTrue(safepath.execution_forbidden(target, untrusted=False))


class RegistryBackupGateTests(unittest.TestCase):
    """用假 winreg 验证：导出失败就不删，导出成功才删。"""

    def _fake_winreg(self, values):
        class Key:
            def __init__(self, store):
                self.store = store

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

        fake = mock.Mock()
        fake.HKEY_CURRENT_USER = object()
        fake.KEY_READ = 1
        fake.KEY_ALL_ACCESS = 2
        store = {name: ("REG_SZ", 1) for name in values}

        def open_key(_root, _path, _reserved, _access):
            return Key(store)

        fake.OpenKey.side_effect = open_key

        def enum_value(_key, i):
            names = list(store)
            if i >= len(names):
                raise OSError("no more values")
            return names[i], "v", 1

        fake.EnumValue.side_effect = enum_value
        fake.EnumKey.side_effect = OSError("no subkeys")

        def delete_value(_key, name):
            store.pop(name, None)

        fake.DeleteValue.side_effect = delete_value
        return fake, store

    def _run_group(self, export_ok):
        values = ["a", "MRUList"]
        fake, store = self._fake_winreg(values)
        target = {"key": "t1", "group": "g", "name": "测试",
                  "path": r"Software\Test\MRU", "mode": "values"}
        with tempfile.TemporaryDirectory() as tmp, \
                mock.patch.object(registry, "winreg", fake), \
                mock.patch.object(registry, "HKCU", fake.HKEY_CURRENT_USER), \
                mock.patch.object(registry, "_export_key", return_value=export_ok), \
                mock.patch.object(registry, "TARGETS", [target]):
            registry.set_backup_root(tmp)
            deleted = registry.group_clean("g")
            registry.set_backup_root(None)
        return deleted, store

    def test_export_failure_blocks_delete(self):
        deleted, store = self._run_group(export_ok=False)
        self.assertEqual(deleted, 0)
        self.assertEqual(sorted(store), ["MRUList", "a"])

    def test_export_success_allows_delete(self):
        deleted, store = self._run_group(export_ok=True)
        self.assertEqual(deleted, 2)
        self.assertEqual(store, {})


class RuleV2Tests(unittest.TestCase):
    def _load(self, doc):
        with tempfile.TemporaryDirectory() as tmp:
            with open(os.path.join(tmp, "rules.json"), "w", encoding="utf-8") as fh:
                json.dump(doc, fh)
            with mock.patch.object(categories, "__file__", os.path.join(tmp, "fake.py")):
                return categories._load_external_rules()

    def test_fields_parsed(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "cache")
            os.makedirs(target)
            doc = {"categories": [{
                "key": "x1", "name": "X", "paths": [target],
                "exclude": ["*.pdf"], "min_age_minutes": 30,
                "process_absent": ["App.exe"], "irreversible_log": True,
            }]}
            cats = self._load(doc)
        self.assertEqual(len(cats), 1)
        self.assertEqual(cats[0].exclude, ("*.pdf",))
        self.assertEqual(cats[0].min_age_minutes, 30)
        self.assertEqual(cats[0].process_absent, ("App.exe",))
        self.assertTrue(cats[0].irreversible_log)

    def test_detect_gates_rule(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "cache")
            os.makedirs(target)
            missing = os.path.join(tmp, "not-there")
            doc = {"categories": [
                {"key": "off", "name": "关", "paths": [target], "detect": [missing]},
                {"key": "on", "name": "开", "paths": [target], "detect": [target]},
            ]}
            cats = self._load(doc)
        self.assertEqual([c.key for c in cats], ["on"])

    def test_scanner_filters(self):
        cat = categories.Category(
            key="t", name="t", tier="standard", kind="files", describe="",
            exclude=("*.pdf",), min_age_minutes=60)
        self.assertTrue(scanner.name_allowed("a.tmp", cat))
        self.assertFalse(scanner.name_allowed("a.pdf", cat))
        now = time.time()
        self.assertFalse(scanner.age_allowed(now - 1800, cat, now=now))
        self.assertTrue(scanner.age_allowed(now - 7200, cat, now=now))

    def test_remote_validation(self):
        base = {"key": "r1", "paths": ["%TEMP%\\x"]}
        ok = dict(base, exclude=["*.pdf"], min_age_minutes=10)
        self.assertEqual(ruleupdate.validate_remote_rule(ok), "")
        for field, value in (("process_absent", ["A.exe"]),
                             ("detect", ["%TEMP%"]),
                             ("os_min", "10.0")):
            bad = dict(base, **{field: value})
            self.assertIn(field, ruleupdate.validate_remote_rule(bad))
        merged, added, rejected, error = ruleupdate.merge_remote(
            {"version": 1, "categories": []},
            {"categories": [dict(base, process_absent=["A.exe"])]})
        self.assertEqual((added, rejected), (0, 1))
        self.assertEqual(error, "")


class ProcessGateTests(unittest.TestCase):
    def test_running_process_skips_category(self):
        with tempfile.TemporaryDirectory() as tmp:
            victim = os.path.join(tmp, "junk.tmp")
            with open(victim, "w") as handle:
                handle.write("data")
            cat = categories.Category(
                key="pg", name="pg", tier="standard", kind="files",
                describe="", paths=[tmp], process_absent=("WeChat.exe",))
            with mock.patch.object(cleaner, "_running_processes",
                                   return_value={"wechat.exe"}):
                result = cleaner.clean_category(cat)
            self.assertIn("正在运行", result.message)
            self.assertTrue(os.path.exists(victim))


class LeftoverMatchTests(unittest.TestCase):
    def test_orphan_detection(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = os.path.join(tmp, "Roaming")
            for name in ("OrphanApp", "KnownApp", "Microsoft", "SuiteParent"):
                os.makedirs(os.path.join(root, name))
            suite_dir = os.path.join(root, "SuiteParent", "Tool")
            installed = [
                {"name": "KnownApp", "publisher": "KnownSoft",
                 "install_location": os.path.join(tmp, "elsewhere", "KnownApp")},
                {"name": "Tool", "publisher": "",
                 "install_location": suite_dir},
            ]
            found = leftover.find_leftovers(installed=installed, roots=[root])
            names = [item["name"] for item in found]
            self.assertEqual(names, ["OrphanApp"])


if __name__ == "__main__":
    unittest.main()
