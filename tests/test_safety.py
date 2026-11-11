"""纯逻辑安全测试。在 Linux 上用临时目录和假注册表运行，不碰真实系统。"""
import os
import shutil
import sqlite3
import tempfile
import unittest
from unittest import mock

import datdecode
import cleaner
import dupes
import scanjobs
import scanner
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
        self.assertIn("https", ruleupdate.validate_url("http://example.com/rules.json"))
        self.assertNotEqual(ruleupdate.validate_url("http://example.com/rules.json"), "")
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
        documents = {"key": "docs", "kind": "files",
                     "paths": [r"%USERPROFILE%\Documents"]}
        self.assertIn("文档", ruleupdate.validate_remote_rule(documents))
        loose = {"key": "video", "kind": "files", "paths": [r"D:\Videos\holiday.mp4"]}
        self.assertIn("临时", ruleupdate.validate_remote_rule(loose))

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


class DatDecodeTests(unittest.TestCase):
    def test_xor_roundtrip_jpeg_png_gif(self):
        import io
        from PIL import Image
        samples = {}
        for fmt, color in (("JPEG", (200, 10, 10)), ("PNG", (10, 180, 40)), ("GIF", (10, 40, 200))):
            image = Image.new("RGB", (6, 6), color)
            buffer = io.BytesIO()
            image.save(buffer, format=fmt)
            samples[fmt] = buffer.getvalue()
        for fmt, raw in samples.items():
            encoded = bytes(byte ^ 0x5A for byte in raw)
            self.assertEqual(datdecode.xor_key(encoded), 0x5A)
            decoded = datdecode.decode_dat(encoded)
            self.assertTrue(decoded.startswith(raw[:4]))
            self.assertEqual(Image.open(io.BytesIO(decoded)).size, (6, 6))

    def test_plain_text_is_not_decoded(self):
        self.assertIsNone(datdecode.xor_key(b"hello world!!"))
        self.assertIsNone(datdecode.decode_dat(b"\x00\x01\x02\x03\x04"))


class ReviewFixTests(unittest.TestCase):
    def test_rollback_keeps_the_only_copy_when_copy_back_fails(self):
        src = tempfile.mkdtemp()
        dest = tempfile.mkdtemp()
        try:
            with open(os.path.join(dest, "only.txt"), "w", encoding="utf-8") as handle:
                handle.write("only-copy")

            def robocopy(_source, _dest, move):
                self.assertFalse(move)
                return 1

            with mock.patch.object(migrate, "_robocopy", side_effect=robocopy):
                with self.assertRaises(migrate.MigrateError):
                    migrate._rollback_to_source(src, dest)
            with open(os.path.join(dest, "only.txt"), encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "only-copy")
        finally:
            shutil.rmtree(src, ignore_errors=True)
            shutil.rmtree(dest, ignore_errors=True)

    def test_incomplete_copy_does_not_delete_source(self):
        src = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp()
        try:
            with open(os.path.join(src, "keep.txt"), "w", encoding="utf-8") as handle:
                handle.write("stay")

            def drive(path):
                full = os.path.abspath(path)
                root = os.path.abspath(dest_root)
                if full == root or full.startswith(root + os.sep):
                    return "D:"
                return "C:"

            def robocopy(_source, dest, move):
                os.makedirs(dest, exist_ok=True)
                return 1

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch.object(migrate, "_robocopy", side_effect=robocopy), \
                    mock.patch.object(migrate, "is_junction", return_value=False):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate(src, dest_root)
            with open(os.path.join(src, "keep.txt"), encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "stay")
        finally:
            shutil.rmtree(src, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_cleaner_refuses_documents_at_execution(self):
        root = tempfile.mkdtemp()
        old = os.environ.get("USERPROFILE")
        os.environ["USERPROFILE"] = root
        try:
            docs = os.path.join(root, "Documents")
            os.makedirs(docs)
            secret = os.path.join(docs, "secret.txt")
            with open(secret, "w", encoding="utf-8") as handle:
                handle.write("keep-me")
            cat = type("C", (), {"key": "t", "name": "t"})()
            result = cleaner.CleanResult(cat)
            cleaner._delete_file(secret, [docs], ("*",), result)
            self.assertTrue(os.path.exists(secret))
            self.assertEqual(result.skipped, 1)
            with open(secret, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "keep-me")
        finally:
            if old is None:
                os.environ.pop("USERPROFILE", None)
            else:
                os.environ["USERPROFILE"] = old
            shutil.rmtree(root, ignore_errors=True)

    def test_shred_symlink_does_not_follow(self):
        folder = tempfile.mkdtemp()
        try:
            target = os.path.join(folder, "secret.txt")
            with open(target, "w", encoding="utf-8") as handle:
                handle.write("safe")
            link = os.path.join(folder, "link.txt")
            os.symlink(target, link)
            with self.assertRaises(shredder.ShredError):
                shredder.shred_path(link)
            with open(target, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "safe")
            nested = os.path.join(folder, "real")
            os.makedirs(nested)
            inside = os.path.join(nested, "data.txt")
            with open(inside, "w", encoding="utf-8") as handle:
                handle.write("dir")
            linkdir = os.path.join(folder, "linkdir")
            os.symlink(nested, linkdir, target_is_directory=True)
            with self.assertRaises(shredder.ShredError):
                shredder.shred_path(linkdir)
            with open(inside, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "dir")
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_shred_rejects_mocked_junction_root(self):
        folder = tempfile.mkdtemp()
        try:
            target = os.path.join(folder, "data.txt")
            with open(target, "w", encoding="utf-8") as handle:
                handle.write("junction")
            with mock.patch.object(safepath, "is_reparse_point", return_value=True):
                with self.assertRaises(shredder.ShredError):
                    shredder.shred_path(target)
            with open(target, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "junction")
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_hidden_duplicate_selection_is_not_deleted(self):
        groups = [
            {"hash": "v", "size": 10, "files": [r"C:\a.mp4", r"C:\b.mp4"],
             "mtimes": {r"C:\a.mp4": 2, r"C:\b.mp4": 1}},
            {"hash": "z", "size": 8, "files": [r"D:\a.zip", r"D:\b.zip"],
             "mtimes": {r"D:\a.zip": 2, r"D:\b.zip": 1}},
        ]
        selected = dupes.auto_select(groups)
        self.assertIn(r"D:\b.zip", selected)
        visible = dupes.paths_for_cleanup(groups, selected, {"video"})
        self.assertNotIn(r"D:\b.zip", visible)
        self.assertNotIn(r"D:\a.zip", visible)
        self.assertTrue(all(path.endswith(".mp4") for path in visible))

    def test_stale_scan_generation_is_dropped(self):
        jobs = scanjobs.ScanJobs()
        first = jobs.start("big")
        second = jobs.start("big")
        self.assertFalse(jobs.accept("big", first))
        self.assertTrue(jobs.accept("big", second))
        self.assertTrue(jobs.accept("dup", jobs.start("dup")))
        self.assertFalse(jobs.accept("im-wechat", 1))

    def test_large_list_is_not_truncated_and_disk_filter_hides_rows(self):
        rows = [(rf"C:\f{i}.mp4", i, 0) for i in range(10)] + [(r"D:\a.zip", 3, 0)]
        self.assertEqual(len(scanner.visible_large_paths(rows, "all", "全部")), 11)
        only_c = scanner.visible_large_paths(rows, "video", "C:")
        self.assertEqual(len(only_c), 10)
        self.assertTrue(all(path.startswith("C:") for path in only_c))
        self.assertEqual(scanner.visible_large_paths(rows, "archive", "全部"), [r"D:\a.zip"])


class ThirdReviewTests(unittest.TestCase):
    def test_windows_temp_secrets_rejected_at_validate_and_execute(self):
        env = {
            "TEMP": r"C:\Users\me\AppData\Local\Temp",
            "TMP": r"C:\Users\me\AppData\Local\Temp",
            "LOCALAPPDATA": r"C:\Users\me\AppData\Local",
            "APPDATA": r"C:\Users\me\AppData\Roaming",
            "USERPROFILE": r"C:\Users\me",
            "SystemRoot": r"C:\Windows",
        }
        self.assertNotEqual(
            ruleupdate.remote_target_allowed(r"C:\Windows\Temp\secrets", env), "")
        self.assertNotEqual(
            ruleupdate.remote_target_allowed(r"%SystemRoot%\Temp\secrets", env), "")
        self.assertNotEqual(
            ruleupdate.remote_target_allowed(r"%TEMP%\..\Windows\Temp\secrets", env), "")
        self.assertNotEqual(
            ruleupdate.remote_target_allowed(r"\\server\share\Temp\secrets", env), "")
        self.assertNotEqual(
            ruleupdate.remote_target_allowed(r"C:\Users\me\AppData\Local\Temp\note.txt:secret", env), "")
        self.assertEqual(
            ruleupdate.remote_target_allowed(r"%TEMP%\Demo\Cache", env), "")

        root = tempfile.mkdtemp()
        old = {key: os.environ.get(key) for key in ("TEMP", "TMP", "LOCALAPPDATA", "SystemRoot", "USERPROFILE")}
        try:
            windows = os.path.join(root, "Windows")
            os.makedirs(os.path.join(windows, "Temp", "secrets"))
            secret = os.path.join(windows, "Temp", "secrets", "keep.txt")
            with open(secret, "w", encoding="utf-8") as handle:
                handle.write("keep")
            user_temp = os.path.join(root, "user-temp")
            os.makedirs(user_temp)
            os.environ["SystemRoot"] = windows
            os.environ["TEMP"] = user_temp
            os.environ["TMP"] = user_temp
            os.environ["LOCALAPPDATA"] = os.path.join(root, "Local")
            os.environ["USERPROFILE"] = os.path.join(root, "profile")
            cat = type("C", (), {
                "key": "remote", "name": "remote", "paths": [os.path.dirname(secret)],
                "kind": "files", "patterns": ("*",), "prune_dirs": False, "untrusted": True,
            })()
            result = cleaner.clean_category(cat)
            self.assertTrue(os.path.exists(secret))
            self.assertEqual(result.deleted, 0)
            with open(secret, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "keep")
        finally:
            for key, value in old.items():
                if value is None:
                    os.environ.pop(key, None)
                else:
                    os.environ[key] = value
            shutil.rmtree(root, ignore_errors=True)

    def test_clean_root_symlink_is_not_followed(self):
        folder = tempfile.mkdtemp()
        try:
            real = os.path.join(folder, "real")
            os.makedirs(real)
            secret = os.path.join(real, "secret.txt")
            with open(secret, "w", encoding="utf-8") as handle:
                handle.write("alive")
            link = os.path.join(folder, "link")
            os.symlink(real, link, target_is_directory=True)
            cat = type("C", (), {
                "key": "t", "name": "t", "paths": [link],
                "kind": "files", "patterns": ("*",), "prune_dirs": False, "untrusted": False,
            })()
            cleaner.clean_category(cat)
            self.assertTrue(os.path.exists(secret))
            with open(secret, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "alive")
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_directory_migrate_keeps_source_if_it_changes_after_copy(self):
        src = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp()
        try:
            original = os.path.join(src, "a.txt")
            with open(original, "w", encoding="utf-8") as handle:
                handle.write("old-content")

            def drive(path):
                full = os.path.abspath(path)
                root = os.path.abspath(dest_root)
                if full == root or full.startswith(root + os.sep):
                    return "D:"
                return "C:"

            def robocopy(source, dest, move):
                os.makedirs(dest, exist_ok=True)
                for name in list(os.listdir(source)):
                    sp = os.path.join(source, name)
                    if os.path.isfile(sp) and not os.path.islink(sp):
                        shutil.copy2(sp, os.path.join(dest, name))
                with open(os.path.join(source, "a.txt"), "w", encoding="utf-8") as handle:
                    handle.write("new-content")
                return 0

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch.object(migrate, "_robocopy", side_effect=robocopy), \
                    mock.patch.object(migrate, "is_junction", return_value=False):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate(src, dest_root)
            with open(original, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "new-content")
            copied = os.path.join(dest_root, os.path.basename(src), "a.txt")
            with open(copied, encoding="utf-8") as handle:
                self.assertEqual(handle.read(), "old-content")
        finally:
            shutil.rmtree(src, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_single_file_migrate_detects_same_size_content_change(self):
        folder = tempfile.mkdtemp()
        dest_root = tempfile.mkdtemp(prefix="dest-")
        try:
            src = os.path.join(folder, "movie.bin")
            with open(src, "wb") as handle:
                handle.write(b"0123456789")

            def drive(path):
                full = os.path.abspath(path)
                root = os.path.abspath(dest_root)
                if full == root or full.startswith(root + os.sep):
                    return "D:"
                return "C:"

            real_copy = shutil.copy2

            def copy2(source, dest, *args, **kwargs):
                real_copy(source, dest, *args, **kwargs)
                with open(source, "wb") as handle:
                    handle.write(b"ABCDEFGHIJ")

            with mock.patch.object(migrate, "_drive_of", side_effect=drive), \
                    mock.patch("migrate.shutil.copy2", side_effect=copy2):
                with self.assertRaises(migrate.MigrateError):
                    migrate.migrate_file(src, dest_root)
            with open(src, "rb") as handle:
                self.assertEqual(handle.read(), b"ABCDEFGHIJ")
            with open(os.path.join(dest_root, "movie.bin"), "rb") as handle:
                self.assertEqual(handle.read(), b"0123456789")
        finally:
            shutil.rmtree(folder, ignore_errors=True)
            shutil.rmtree(dest_root, ignore_errors=True)

    def test_shred_failure_is_recorded_as_failed(self):
        folder = tempfile.mkdtemp()
        try:
            records = shredder.run_batch(
                [os.path.join(folder, "missing.bin")],
                shred_fn=lambda path: (_ for _ in ()).throw(shredder.ShredError("失败")),
            )
            self.assertEqual(records[0]["status"], "failed")
            history.append_shred_history(folder, records)
            rows = history.load_shred_history(folder)
            self.assertEqual(rows[0]["status"], "failed")
            self.assertNotEqual(rows[0]["status"], "ok")
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_rules_write_crash_keeps_previous_file(self):
        folder = tempfile.mkdtemp()
        path = os.path.join(folder, "rules.json")
        try:
            with open(path, "w", encoding="utf-8") as handle:
                handle.write('{"version": 1, "categories": [{"key": "old", "kind": "files"}]}\n')
            with mock.patch.object(ruleupdate, "RULES_PATH", path), \
                    mock.patch("ruleupdate.os.replace", side_effect=OSError("crash")):
                added, _total, err = ruleupdate.update_rules(
                    "https://example.com/rules.json",
                    opener=_OkOpener(b'{"categories": [{"key": "app_demo", "kind": "files", "paths": ["%TEMP%\\\\Cache"]}]}'),
                )
            self.assertNotEqual(added, 1)
            with open(path, encoding="utf-8") as handle:
                self.assertIn("old", handle.read())
        finally:
            shutil.rmtree(folder, ignore_errors=True)

    def test_privacy_count_without_action_is_not_selectable(self):
        record = {"id": "risk_money", "action": "files", "count": 4, "files": []}
        self.assertFalse(privacy.is_selectable(record))
        shop = {"id": "shop", "action": "sql", "count": 2, "files": [], "sql_jobs": [{"path": "h"}]}
        self.assertTrue(privacy.is_selectable(shop))


class _OkOpener:
    def __init__(self, payload):
        self.payload = payload

    def open(self, url, timeout=15):
        payload = self.payload

        class Resp:
            def read(self, n):
                return payload

            def __enter__(self):
                return self

            def __exit__(self, *args):
                return False

        return Resp()


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
