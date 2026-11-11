"""外部评审 5 条发现的修复回归测试。

#1 注册表残留误判功能性子键；#2 残留目录永久删除改回收站；
#3 注册表删除中途失败自动回滚；#4 mtime 取不到时按“够旧”误删；
#5 空目录裁剪绕过永不清理硬名单。
"""
import os
import tempfile
import unittest
from unittest import mock

import categories
import cleaner
import leftover
import recyclebin
import registry
import utils


class TelemetrySkipTests(unittest.TestCase):
    def test_functional_subkeys_not_flagged(self):
        views = [{"hive": "HKCU", "prefix": "Software",
                  "vendors": {"VendorX": ["Telemetry", "ProductApp"]}}]
        found = leftover.match_registry_leftovers(views, [], set())
        paths = [item["path"] for item in found]
        self.assertNotIn(r"Software\VendorX\Telemetry", paths)
        self.assertIn(r"Software\VendorX\ProductApp", paths)


class MtimeUnknownTests(unittest.TestCase):
    def test_unknown_mtime_skips_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            target = os.path.join(tmp, "old.bin")
            with open(target, "wb") as fh:
                fh.write(b"x" * 8)
            cat = categories.Category(
                key="t", name="t", tier="standard", kind="files", describe="",
                paths=[tmp], min_age_minutes=1440)
            result = cleaner.CleanResult(cat)
            with mock.patch.object(cleaner.os.path, "getmtime",
                                   side_effect=OSError("gone")):
                cleaner._delete_file(target, [utils.norm_path(tmp)], ("*",),
                                     result, False, cat)
            self.assertTrue(os.path.exists(target))
            self.assertEqual(result.skipped, 1)
            self.assertEqual(result.deleted, 0)


class PruneGuardTests(unittest.TestCase):
    def test_never_clean_dirs_not_pruned(self):
        with tempfile.TemporaryDirectory() as tmp:
            protected = os.path.join(tmp, "protected")
            normal = os.path.join(tmp, "normal")
            os.makedirs(protected)
            os.makedirs(normal)
            removed = utils.remove_empty_dirs(
                tmp, skip=lambda path: path == protected)
            self.assertEqual(removed, 1)
            self.assertTrue(os.path.isdir(protected))
            self.assertFalse(os.path.exists(normal))


class RestoreOnFailureTests(unittest.TestCase):
    def test_cleaner_restores_when_delete_fails(self):
        cat = categories.Category(
            key="reg_leftovers", name="t", tier="deep", kind="regleftover",
            describe="", paths=[r"HKCU::Software\Ghost\GhostApp"])
        with mock.patch.object(registry, "backup_hive_key", return_value=True), \
                mock.patch.object(registry, "delete_tree_at", return_value=False), \
                mock.patch.object(registry, "restore_backup", return_value=True) as restore, \
                mock.patch.object(registry, "last_backup_info",
                                  return_value={"dir": "D", "failed": []}):
            result = cleaner.clean_category(cat)
        restore.assert_called_once_with("HKCU", r"Software\Ghost\GhostApp")
        self.assertEqual(result.deleted, 0)
        self.assertTrue(any("已从备份恢复" in e for e in result.errors))

    def test_restore_backup_uses_recorded_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = os.path.join(tmp, "Ghost.reg")
            with open(dest, "w", encoding="utf-8") as fh:
                fh.write("Windows Registry Editor Version 5.00\n")
            registry._BACKUP_FILES[("HKCU", r"Software\Ghost")] = dest
            try:
                proc = mock.Mock(returncode=0)
                with mock.patch.object(registry.subprocess, "run",
                                       return_value=proc) as run:
                    self.assertTrue(registry.restore_backup("HKCU", r"Software\Ghost"))
                args = run.call_args[0][0]
                self.assertEqual(args[:2], ["reg.exe", "import"])
                self.assertEqual(args[2], dest)
            finally:
                registry._BACKUP_FILES.pop(("HKCU", r"Software\Ghost"), None)


class RecycleLeftoversTests(unittest.TestCase):
    def test_leftover_dir_goes_to_recycle_not_unlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            orphan = os.path.join(tmp, "OrphanApp")
            os.makedirs(orphan)
            with open(os.path.join(orphan, "data.txt"), "w") as fh:
                fh.write("user data")
            cat = categories.Category(
                key="app_leftovers", name="t", tier="deep", kind="files",
                describe="", paths=[orphan], recycle=True)
            with mock.patch.object(recyclebin, "send_to_recycle",
                                   return_value=[]) as send:
                result = cleaner.clean_category(cat)
            send.assert_called_once_with([orphan])
            # 模拟实现没有真的移走（mock），但 cleaner 必须按“已移走”记账，
            # 且绝不允许自己再做永久删除：目录与文件都应原样还在。
            self.assertTrue(os.path.exists(os.path.join(orphan, "data.txt")))
            self.assertEqual(result.deleted, 1)
            self.assertGreater(result.freed, 0)

    def test_recycle_failure_keeps_dir_and_reports(self):
        with tempfile.TemporaryDirectory() as tmp:
            orphan = os.path.join(tmp, "OrphanApp")
            os.makedirs(orphan)
            cat = categories.Category(
                key="app_leftovers", name="t", tier="deep", kind="files",
                describe="", paths=[orphan], recycle=True)
            with mock.patch.object(recyclebin, "send_to_recycle",
                                   return_value=[orphan]):
                result = cleaner.clean_category(cat)
            self.assertEqual(result.deleted, 0)
            self.assertEqual(result.skipped, 1)
            self.assertTrue(os.path.isdir(orphan))


if __name__ == "__main__":
    unittest.main()
