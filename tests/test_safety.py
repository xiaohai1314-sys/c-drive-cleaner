"""纯逻辑安全测试。在 Linux 上用临时目录和假注册表运行，不碰真实系统。"""
import os
import shutil
import sqlite3
import tempfile
import unittest
from unittest import mock

import cleaner
import dupes
import fragviz
import history
import imclean
import migrate
import privacy
import ruleupdate
import safepath
import shredder


class PathProtectionTests(unittest.TestCase):
    def test_system_and_drive_roots_are_blocked(self):
        blocked = [
            r"C:\Windows\System32\cmd.exe",
            r"C:\Windows\SysWOW64\notepad.exe",
            r"C:\Program Files\App\app.exe",
            r"C:\Program Files (x86)\App\app.exe",
            r"C:\ProgramData\Microsoft\crypto",
            r"C:\pagefile.sys",
            r"C:\hiberfil.sys",
            r"C:\\",
            r"D:\System Volume Information\tracking.log",
        ]
        for path in blocked:
            self.assertTrue(safepath.is_protected_path(path), path)

    def test_user_file_is_not_a_system_path(self):
        self.assertFalse(safepath.is_protected_path(r"C:\Users\me\Videos\big.mp4"))
        self.assertFalse(safepath.is_protected_path(r"D:\data\notes.txt"))

    def test_profile_root_blocked_children_allowed(self):
        old = os.environ.get("USERPROFILE")
        os.environ["USERPROFILE"] = r"C:\Users\me"
        try:
            self.assertTrue(safepath.is_protected_path(r"C:\Users\me"))
            self.assertFalse(safepath.is_protected_path(r"C:\Users\me\Desktop\a.txt"))
        finally:
            if old is None:
                os.environ.pop("USERPROFILE", None)
            else:
                os.environ["USERPROFILE"] = old

    def test_shredder_refuses_system_path_before_delete(self):
        with self.assertRaises(shredder.ShredError):
            shredder.shred_path(r"C:\Windows\System32\cmd.exe")

    def test_libraries_are_not_migration_candidates(self):
        env = {"USERPROFILE": r"C:\Users\me"}
        for path in (
            r"C:\Users\me\Documents",
            r"C:\Users\me\Desktop",
            r"C:\Users\me\Downloads",
            r"C:\Users\me\Downloads\Setup.exe",
            r"C:\Users\me\Documents\WeChat Files",
        ):
            self.assertTrue(safepath.is_user_library(path, env), path)
            self.assertTrue(safepath.migration_forbidden(path, env), path)
        self.assertFalse(safepath.migration_forbidden(r"C:\Program Files\Demo", env))
        self.assertTrue(safepath.migration_forbidden(r"C:\Windows\System32", env))
        self.assertTrue(safepath.migration_forbidden(r"C:\Program Files", env))

    def test_password_store_names(self):
        self.assertTrue(safepath.is_password_store(
            r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\Login Data"))
        self.assertTrue(safepath.is_password_store(
            r"C:\Users\a\AppData\Local\Quark\User Data\Default\Login Data-journal"))
        self.assertTrue(safepath.is_password_store(
            r"C:\Users\a\AppData\Roaming\Mozilla\Firefox\Profiles\x\logins.json"))
        self.assertFalse(safepath.is_password_store(
            r"C:\Users\a\AppData\Local\Google\Chrome\User Data\Default\History"))

    def test_cleaner_skips_password_database(self):
        folder = tempfile.mkdtemp()
        try:
            secret = os.path.join(folder, "Login Data")
            normal = os.path.join(folder, "cache.tmp")
            with open(secret, "w", encoding="utf-8") as handle:
                handle.write("secret")
            with open(normal, "w", encoding="utf-8") as handle:
                handle.write("junk")
            cat = type("C", (), {"key": "t", "name": "t"})()
            result = cleaner.CleanResult(cat)
            cleaner._delete_file(secret, [folder], ("*",), result)
            self.assertTrue(os.path.exists(secret))
            self.assertEqual(result.skipped, 1)
            other = cleaner.CleanResult(cat)
            cleaner._delete_file(normal, [folder], ("*",), other)
            self.assertFalse(os.path.exists(normal))
            self.assertEqual(other.deleted, 1)
        finally:
            shutil.rmtree(folder, ignore_errors=True)


class DedupeKeepTests(unittest.TestCase):
    def _groups(self):
        return [
            {"hash": "a", "size": 10, "files": [r"C:\a", r"C:\b", r"C:\c"],
             "mtimes": {r"C:\a": 1, r"C:\b": 9, r"C:\c": 3}},
            {"hash": "d", "size": 4, "files": [r"D:\d1", r"D:\d2"],
             "mtimes": {r"D:\d1": 2, r"D:\d2": 2}},
        ]

    def test_auto_select_keeps_one_per_group(self):
        groups = self._groups()
        chosen = dupes.auto_select(groups)
        self.assertNotIn(r"C:\b", chosen)
        self.assertIn(r"C:\a", chosen)
        self.assertIn(r"C:\c", chosen)
        self.assertEqual(len([p for p in chosen if p.startswith("C:")]), 2)
        self.assertEqual(len([p for p in chosen if p.startswith("D:")]), 1)

    def test_selecting_all_still_keeps_one(self):
        groups = self._groups()
        everything = [path for group in groups for path in group["files"]]
        doomed = dupes.paths_to_delete(groups, everything)
        self.assertNotIn(r"C:\b", doomed)
        self.assertNotIn(r"D:\d2", doomed)
        self.assertEqual(len(doomed), 3)
        for group in groups:
            left = [path for path in group["files"] if path not in doomed]
            self.assertEqual(len(left), 1)

    def test_partial_selection_is_respected(self):
        groups = self._groups()
        doomed = dupes.paths_to_delete(groups, [r"C:\a"])
        self.assertEqual(doomed, [r"C:\a"])
        self.assertEqual(dupes.paths_to_delete(groups, []), [])


class ImcleanTests(unittest.TestCase):
    def test_dat_and_extensionless_follow_media_directories(self):
        cases = {
            r"D:\xwechat_files\wxid\msg\video\2e7269d.t.dat": ("video", ""),
            r"D:\xwechat_files\wxid\msg\video\abc": ("video", ""),
            r"D:\xwechat_files\wxid\msg\attach\pic.dat": ("image", ""),
            r"D:\xwechat_files\wxid\msg\file\report.docx": ("file", "doc"),
            r"D:\xwechat_files\wxid\msg\file\clip.mp4": ("file", "video"),
            r"D:\xwechat_files\wxid\msg\file\pack.zip": ("file", "archive"),
            r"D:\xwechat_files\wxid\cache\foo.dat": ("cache", ""),
            r"D:\xwechat_files\wxid\msg\voice\a.silk": ("audio", ""),
            r"D:\WeChat Files\wxid\FileStorage\Image\2024-11\a.jpg": ("image", ""),
        }
        for path, expected in cases.items():
            self.assertEqual(imclean.classify(path), expected, path)

    def test_databases_and_loose_dat_are_skipped(self):
        skipped = [
            r"D:\xwechat_files\wxid\config.dat",
            r"D:\xwechat_files\wxid\db_storage\message\msg.db",
            r"D:\xwechat_files\wxid\db_storage\message\extra.dat",
            r"D:\xwechat_files\wxid\config\config.json",
            r"D:\xwechat_files\wxid\msg\video\index.db",
            r"D:\Tencent Files\1\nt_qq\database\a.dat",
        ]
        for path in skipped:
            kind, _sub = imclean.classify(path)
            self.assertIsNone(kind, path)

    def test_clean_only_removes_classified_media(self):
        root = tempfile.mkdtemp()
        try:
            video = os.path.join(root, "msg", "video")
            db = os.path.join(root, "db_storage")
            os.makedirs(video)
            os.makedirs(db)
            dat = os.path.join(video, "clip.dat")
            database = os.path.join(db, "msg.db")
            loose = os.path.join(root, "config.dat")
            for path in (dat, database, loose):
                with open(path, "wb") as handle:
                    handle.write(b"1234")

            def remover(paths):
                for path in paths:
                    os.remove(path)
                return []

            freed, deleted = imclean.clean(
                [root], {"video", "image", "file", "cache"},
                paths=[dat, database, loose], remover=remover)
            self.assertFalse(os.path.exists(dat))
            self.assertTrue(os.path.exists(database))
            self.assertTrue(os.path.exists(loose))
            self.assertEqual(deleted, 1)
            self.assertGreater(freed, 0)
        finally:
            shutil.rmtree(root, ignore_errors=True)


class RuleValidationTests(unittest.TestCase):
    def test_url_and_command_rules(self):
        self.assertEqual(ruleupdate.validate_url("https://example.com/rules.json"), "")
        self.assertTrue(ruleupdate.validate_url("file:///tmp/rules.json"))
        self.assertTrue(ruleupdate.validate_url("javascript:alert(1)"))
        command = {"key": "evil", "kind": "command", "command": ["cmd.exe", "/c", "calc"]}
        self.assertIn("command", ruleupdate.validate_remote_rule(command))
        login = {"key": "pw", "kind": "files",
                 "paths": [r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Login Data"]}
        self.assertIn("密码", ruleupdate.validate_remote_rule(login))
        traversal = {"key": "up", "kind": "files", "paths": [r"%TEMP%\..\Windows\System32"]}
        self.assertTrue(ruleupdate.validate_remote_rule(traversal))
        ok = {"key": "app_demo", "kind": "files", "tier": "standard",
              "paths": [r"%TEMP%\Demo\Cache"]}
        self.assertEqual(ruleupdate.validate_remote_rule(ok), "")

    def test_merge_keeps_local_command_and_drops_remote_command(self):
        local = {"version": 1, "categories": [
            {"key": "defrag", "kind": "command", "command": ["defrag.exe", "C:", "/O"]},
        ]}
        remote = {"categories": [
            {"key": "evil", "kind": "command", "command": ["powershell.exe"]},
            {"key": "app_demo", "name": "示例缓存", "kind": "files",
             "paths": [r"%TEMP%\Demo\Cache"]},
            {"key": "defrag", "kind": "command", "command": ["calc.exe"]},
        ]}
        merged, added, rejected, error = ruleupdate.merge_remote(local, remote)
        self.assertEqual(error, "")
        self.assertEqual(added, 1)
        self.assertEqual(rejected, 2)
        keys = {item["key"]: item for item in merged["categories"]}
        self.assertEqual(keys["defrag"]["kind"], "command")
        self.assertNotIn("evil", keys)
        self.assertEqual(keys["app_demo"]["kind"], "files")
        self.assertNotIn("command", keys["app_demo"])

    def test_update_rules_rejects_bad_url_without_writing(self):
        added, _total, err = ruleupdate.update_rules("file:///tmp/rules.json")
        self.assertEqual(added, 0)
        self.assertIn("http", err)

    def test_shipped_rules_do_not_delete_login_data(self):
        root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
        with open(os.path.join(root, "rules.json"), encoding="utf-8") as handle:
            text = handle.read()
        self.assertNotIn("\\\\Login Data", text)
        self.assertNotIn("logins.json", text.lower())


class MigrateRollbackTests(unittest.TestCase):
    def test_junction_failure_restores_source(self):
        src = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp()
        try:
            with open(os.path.join(src, "a.txt"), "w", encoding="utf-8") as handle:
                handle.write("hello-data")

            def drive(path):
                full = os.path.abspath(path)
                dest = os.path.abspath(dest_root)
                if full == dest or full.startswith(dest + os.sep):
                    return "D:"
                return "C:"

            def robocopy(source, dest, move):
                os.makedirs(dest, exist_ok=True)
                for name in list(os.listdir(source)):
                    sp = os.path.join(source, name)
                    dp = os.path.join(dest, name)
                    if os.path.isfile(sp):
                        shutil.copy2(sp, dp)
                        if move:
                            os.remove(sp)
                return 1

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch.object(migrate, "_robocopy", side_effect=robocopy), \
                    mock.patch.object(migrate, "create_junction",
                                      side_effect=lambda link, target: os.makedirs(link, exist_ok=True)), \
                    mock.patch.object(migrate, "junction_works", return_value=False), \
                    mock.patch.object(migrate, "is_junction", return_value=False):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate(src, dest_root)
            with open(os.path.join(src, "a.txt"), encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "hello-data")
            self.assertFalse(os.path.isdir(os.path.join(dest_root, os.path.basename(src))))
        finally:
            shutil.rmtree(src, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_copy_failure_does_not_remove_source(self):
        src = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp()
        try:
            with open(os.path.join(src, "a.txt"), "w", encoding="utf-8") as handle:
                handle.write("keep")

            def drive(path):
                full = os.path.abspath(path)
                dest = os.path.abspath(dest_root)
                if full == dest or full.startswith(dest + os.sep):
                    return "D:"
                return "C:"

            def robocopy(source, dest, move):
                os.makedirs(dest, exist_ok=True)
                with open(os.path.join(dest, "partial.txt"), "w", encoding="utf-8") as handle:
                    handle.write("x")
                return 16

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch.object(migrate, "_robocopy", side_effect=robocopy), \
                    mock.patch.object(migrate, "is_junction", return_value=False):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate(src, dest_root)
            self.assertTrue(os.path.isfile(os.path.join(src, "a.txt")))
            self.assertFalse(os.path.exists(os.path.join(dest_root, os.path.basename(src))))
        finally:
            shutil.rmtree(src, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_file_link_failure_rolls_back(self):
        folder = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp(prefix="dest-")
        try:
            src = os.path.join(folder, "movie.mp4")
            with open(src, "wb") as handle:
                handle.write(b"0123456789")

            def drive(path):
                full = os.path.abspath(path)
                dest = os.path.abspath(dest_root)
                if full == dest or full.startswith(dest + os.sep):
                    return "D:"
                return "C:"

            def linker(_dest, _src):
                raise OSError("symlink denied")

            with mock.patch.object(migrate, "_drive_of", side_effect=drive):
                with self.assertRaises(migrate.MigrateError) as caught:
                    migrate.migrate_file(src, dest_root, linker=linker)
            self.assertIn("还原", str(caught.exception))
            with open(src, "rb") as handle:
                self.assertEqual(handle.read(), b"0123456789")
            self.assertFalse(os.path.exists(os.path.join(dest_root, "movie.mp4")))
        finally:
            shutil.rmtree(folder, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_file_size_mismatch_keeps_source(self):
        folder = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp(prefix="dest-")
        try:
            src = os.path.join(folder, "movie.mp4")
            with open(src, "wb") as handle:
                handle.write(b"0123456789")

            def drive(path):
                full = os.path.abspath(path)
                dest = os.path.abspath(dest_root)
                if full == dest or full.startswith(dest + os.sep):
                    return "D:"
                return "C:"

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch.object(migrate, "_same_file_size", return_value=False):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate_file(src, dest_root)
            self.assertTrue(os.path.isfile(src))
            self.assertFalse(os.path.exists(os.path.join(dest_root, "movie.mp4")))
        finally:
            shutil.rmtree(folder, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_discover_skips_documents_desktop_downloads(self):
        class Key:
            def __init__(self, subs=None, values=None):
                self.subs = subs or {}
                self.values = values or {}

            def Close(self):
                return None

        class FakeReg:
            HKEY_LOCAL_MACHINE = "HKLM"
            HKEY_CURRENT_USER = "HKCU"

            def __init__(self, tree):
                self.tree = tree

            def OpenKey(self, root, path):
                if isinstance(root, Key):
                    key = root.subs.get(path)
                else:
                    key = self.tree.get((root, path))
                if key is None:
                    raise OSError(path)
                return key

            def EnumKey(self, key, index):
                names = list(key.subs)
                if index >= len(names):
                    raise OSError("end")
                return names[index]

            def EnumValue(self, key, index):
                names = list(key.values)
                if index >= len(names):
                    raise OSError("end")
                name = names[index]
                return name, key.values[name], 1

        uninstall = Key(subs={
            "demo": Key(values={
                "DisplayName": "演示软件",
                "InstallLocation": r"C:\Program Files\Demo",
                "EstimatedSize": 12,
            }),
            "docs": Key(values={
                "DisplayName": "文档",
                "InstallLocation": r"C:\Users\me\Documents",
                "EstimatedSize": 99,
            }),
            "desk": Key(values={
                "DisplayName": "桌面",
                "InstallLocation": r"C:\Users\me\Desktop",
            }),
            "down": Key(values={
                "DisplayName": "下载",
                "InstallLocation": r"C:\Users\me\Downloads\Portable",
            }),
            "patch": Key(values={
                "DisplayName": "更新",
                "InstallLocation": r"C:\Program Files\Demo",
                "ReleaseType": "Update",
            }),
        })
        reg = FakeReg({
            ("HKLM", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"): uninstall,
        })
        rows = migrate.discover_installed(
            reg=reg,
            environ={"USERPROFILE": r"C:\Users\me"},
            exists=lambda _path: True,
        )
        paths = [row["path"] for row in rows]
        self.assertEqual(paths, [r"C:\Program Files\Demo"])
        self.assertEqual(rows[0]["size"], 12 * 1024)


class PrivacyAndHistoryTests(unittest.TestCase):
    def test_trace_list_has_no_password_files(self):
        names = {name.lower() for name in privacy.TRACE_FILENAMES}
        self.assertNotIn("login data", names)
        self.assertNotIn("logins.json", names)

    def test_history_sql_keeps_unrelated_rows(self):
        folder = tempfile.mkdtemp()
        try:
            profile = os.path.join(folder, "Default")
            os.makedirs(profile)
            history_db = os.path.join(profile, "History")
            login = os.path.join(profile, "Login Data")
            with open(login, "w", encoding="utf-8") as handle:
                handle.write("passwords")
            conn = sqlite3.connect(history_db)
            conn.execute("CREATE TABLE urls (id INTEGER PRIMARY KEY, url TEXT)")
            conn.execute("CREATE TABLE visits (id INTEGER PRIMARY KEY, url INTEGER)")
            conn.execute("CREATE TABLE downloads (id INTEGER PRIMARY KEY, target_path TEXT)")
            conn.execute("CREATE TABLE downloads_url_chains (id INTEGER, chain_id INTEGER)")
            conn.execute("INSERT INTO urls (url) VALUES ('https://example.com/a')")
            conn.execute("INSERT INTO urls (url) VALUES ('https://item.taobao.com/1')")
            conn.execute("INSERT INTO visits (url) VALUES (1)")
            conn.execute("INSERT INTO visits (url) VALUES (2)")
            conn.execute("INSERT INTO downloads (target_path) VALUES ('a.zip')")
            conn.commit()
            conn.close()

            picked = privacy.collect_chromium_files(profile)
            self.assertIn(history_db, picked)
            self.assertNotIn(login, picked)
            stats = privacy.history_stats(history_db)
            self.assertEqual(stats["urls"], 2)
            self.assertEqual(stats["downloads"], 1)
            self.assertEqual(stats["shop"], 1)

            self.assertTrue(privacy.apply_sql(history_db, privacy._download_sql(history_db)))
            stats = privacy.history_stats(history_db)
            self.assertEqual(stats["downloads"], 0)
            self.assertEqual(stats["urls"], 2)

            self.assertTrue(privacy.apply_sql(history_db, privacy._shop_sql()))
            stats = privacy.history_stats(history_db)
            self.assertEqual(stats["shop"], 0)
            self.assertEqual(stats["urls"], 1)
            self.assertFalse(privacy.apply_sql(login, ["DELETE FROM urls"]))
            with open(login, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "passwords")

            records = [{
                "id": "hist_chrome", "section": "history", "browser": "chrome",
                "action": "files", "files": [history_db, login],
            }]
            plan = privacy.clean_plan(records, ["hist_chrome"])
            self.assertIn(history_db, plan["files"])
            self.assertNotIn(login, plan["files"])
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_history_log_roundtrip(self):
        folder = tempfile.mkdtemp()
        try:
            history.write_log(folder, [
                "开始清理",
                "清理：临时文件",
                "清理结束，共释放 1.5 MB",
            ])
            rows = history.load_history(folder)
            self.assertEqual(len(rows), 1)
            self.assertEqual(rows[0]["freed"], "1.5 MB")
            self.assertEqual(rows[0]["items"], 1)
        finally:
            shutil.rmtree(folder, ignore_errors=True)


class FragVizTests(unittest.TestCase):
    def test_grid_follows_real_percentage(self):
        text = """
        可用空间                 = 56.00 GB
        碎片总计                 = 33%
        碎片文件                 = 18.99 GB
        碎片数                   = 135895
        """
        parsed = fragviz.parse_defrag_report(text)
        self.assertEqual(parsed["percent"], 33.0)
        self.assertEqual(parsed["space_gb"], 18.99)
        self.assertEqual(parsed["count"], 135895)
        self.assertIsNone(fragviz.cells_from_analysis(None, 4, 8))
        cells = fragviz.cells_from_analysis(50, 2, 10)
        self.assertEqual(cells.count("hot"), 10)
        self.assertEqual(fragviz.cells_from_analysis(0, 3, 4).count("hot"), 0)
        self.assertEqual(len(fragviz.cells_from_analysis(33, 11, 32)), 11 * 32)


if __name__ == "__main__":
    unittest.main()
