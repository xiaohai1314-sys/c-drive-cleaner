"""注册表残留清理的回归测试：纯比对逻辑、备份门、删除确认。

在 Linux 上用假数据与假 winreg 运行，不碰真实注册表。
"""
import unittest
from unittest import mock

import categories
import cleaner
import leftover
import registry
import scanner


class MatchRegistryLeftoversTests(unittest.TestCase):
    VIEWS = [{
        "hive": "HKCU", "prefix": "Software",
        "vendors": {
            "Tencent": ["QQ", "WeChat", "Common"],
            "Microsoft": ["OneDrive"],
            "Classes": ["Anything"],
            "GhostSoft": ["GhostApp"],
            "SoloApp": [],
            "PortableSoft": ["PortableApp"],
        },
    }]
    INSTALLED = [
        {"name": "QQ", "publisher": "Tencent", "install_location": ""},
        {"name": "WeChat", "publisher": "Tencent", "install_location": ""},
    ]

    def test_only_true_orphans_listed(self):
        found = leftover.match_registry_leftovers(self.VIEWS, self.INSTALLED, set())
        paths = [(item["hive"], item["path"]) for item in found]
        self.assertIn(("HKCU", r"Software\GhostSoft\GhostApp"), paths)
        self.assertIn(("HKCU", r"Software\SoloApp"), paths)
        self.assertIn(("HKCU", r"Software\PortableSoft\PortableApp"), paths)
        # 已安装的应用、系统区、共享名子键都不出现
        self.assertNotIn(("HKCU", r"Software\Tencent\QQ"), paths)
        self.assertNotIn(("HKCU", r"Software\Tencent\WeChat"), paths)
        self.assertNotIn(("HKCU", r"Software\Tencent\Common"), paths)
        self.assertFalse(any("Microsoft" in path for _hive, path in paths))
        self.assertFalse(any("Classes" in path for _hive, path in paths))

    def test_live_directory_suppresses_candidate(self):
        found = leftover.match_registry_leftovers(
            self.VIEWS, self.INSTALLED, {"portableapp"})
        paths = [item["path"] for item in found]
        self.assertNotIn(r"Software\PortableSoft\PortableApp", paths)
        self.assertIn(r"Software\GhostSoft\GhostApp", paths)


class _FakeKey:
    def __init__(self, node):
        self.node = node

    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class _FakeWinreg:
    """嵌套 dict 假注册表：{键名: {子键名: ...}}，值不建模。"""

    HKEY_CURRENT_USER = "HKCU"
    HKEY_LOCAL_MACHINE = "HKLM"
    KEY_READ = 1
    KEY_ALL_ACCESS = 2

    def __init__(self):
        self.trees = {"HKCU": {}, "HKLM": {}}

    def _node(self, root, path):
        node = self.trees[root]
        if path:
            for part in path.split("\\"):
                node = node[part]
        return node

    def OpenKey(self, root, path, _reserved=0, _access=0):
        try:
            return _FakeKey(self._node(root, path))
        except KeyError:
            raise OSError("missing")

    def EnumKey(self, key, index):
        names = list(key.node)
        if index >= len(names):
            raise OSError("no more")
        return names[index]

    def DeleteKey(self, parent_key, subkey):
        if subkey not in parent_key.node:
            raise OSError("missing")
        del parent_key.node[subkey]


class DeleteTreeAtTests(unittest.TestCase):
    def test_delete_tree_at_removes_recursively(self):
        fake = _FakeWinreg()
        fake.trees["HKLM"] = {"Software": {"Old": {"Thing": {"Sub": {}}}}}
        with mock.patch.object(registry, "winreg", fake):
            self.assertTrue(registry.delete_tree_at("HKLM", r"Software\Old\Thing"))
            self.assertFalse(registry.delete_tree_at("HKLM", r"Software\Old\Thing"))
        self.assertEqual(fake.trees["HKLM"], {"Software": {"Old": {}}})

    def test_backup_hive_key_export_failure(self):
        fake = _FakeWinreg()
        fake.trees["HKLM"] = {"Software": {"Old": {}}}
        with mock.patch.object(registry, "winreg", fake), \
                mock.patch.object(registry, "_export_key", return_value=False):
            self.assertFalse(registry.backup_hive_key("HKLM", r"Software\Old", "Old"))
            self.assertTrue(registry.backup_hive_key("HKLM", r"Software\Gone", "Gone"))


class CleanRegLeftoversTests(unittest.TestCase):
    def test_backup_gate_in_cleaner(self):
        cat = categories.Category(
            key="reg_leftovers", name="t", tier="deep", kind="regleftover",
            describe="", paths=[r"HKCU::Software\GhostSoft\GhostApp",
                                r"HKLM::Software\Old\Thing"])

        def fake_backup(hive, _rel, _label):
            return hive == "HKCU"

        with mock.patch.object(registry, "backup_hive_key", side_effect=fake_backup), \
                mock.patch.object(registry, "delete_tree_at", return_value=True) as deleter, \
                mock.patch.object(registry, "last_backup_info",
                                  return_value={"dir": "D", "failed": []}):
            result = cleaner.clean_category(cat)
        self.assertEqual(result.deleted, 1)
        self.assertEqual(result.skipped, 1)
        deleter.assert_called_once_with("HKCU", r"Software\GhostSoft\GhostApp")

    def test_scan_counts_keys(self):
        cat = categories.Category(
            key="reg_leftovers", name="t", tier="deep", kind="regleftover",
            describe="", paths=["HKCU::Software\\A\\B", "HKLM::Software\\C\\D"])
        self.assertEqual(scanner.scan_category(cat), (-1, 2))


if __name__ == "__main__":
    unittest.main()
