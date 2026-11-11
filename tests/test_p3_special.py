"""P3 专项：出厂规则不变量 + 执行门审计 + 加载器与安全逻辑补强。

背景：rules.json 的条目一律以 untrusted 身份加载，执行时要过
safepath.under_remote_allowlist。路径段名不在缓存词表里的规则会
“能扫不能删”（静默跳过）。本文件把这条约束钉成测试：
- P3 新增规则必须 100% 过门；
- 存量过不了门的路径登记为已知集合，只许缩小，不许悄悄扩大。
"""
import json
import os
import tempfile
import unittest
from unittest import mock

import categories
import cleaner
import leftover
import recyclebin
import registry
import ruleupdate
import safepath
import scanner
import utils

FAKE_ENV = {
    "LOCALAPPDATA": r"C:\Users\T\AppData\Local",
    "APPDATA": r"C:\Users\T\AppData\Roaming",
    "USERPROFILE": r"C:\Users\T",
    "TEMP": r"C:\Users\T\AppData\Local\Temp",
    "TMP": r"C:\Users\T\AppData\Local\Temp",
    "SystemRoot": r"C:\Windows",
}

P3_KEYS = set("""
app_chrome_cache app_edge_cache app_chrome_beta app_firefox app_waterfox
app_thunderbird app_brave app_opera app_vivaldi app_qqbrowser app_360browser
app_centbrowser app_quark app_telegram app_discord app_slack app_zoom
app_skype app_line app_signal app_element app_rocketchat app_mattermost
app_vscode app_vscode_insiders app_cursor app_claude app_jetbrains
app_androidstudio app_visualstudio app_npm app_yarn app_pip app_uv
app_gobuild app_nuget app_docker app_postman app_insomnia app_apifox
app_compass app_github_desktop app_sublime app_wxdevtools app_unity
app_epic app_origin app_ubisoft app_battlenet app_riot app_itch app_obs
app_handbrake app_capcut app_notion app_obsidian app_logseq app_typora
app_siyuan app_yuque app_ynote app_canva app_drawio app_mubu app_utools
app_douyin app_doubao app_wps app_libreoffice app_onlyoffice app_zotero
app_onedrive_logs app_todesk app_nvidia_shader app_amd_shader app_adrive
""".split())

# 存量规则中过不了执行门的路径（类别, 路径模板）。成因：隐私文件由
# 隐私专页处理、Documents 下的用户库目录被硬拦、SystemRoot 系统区、
# 非缓存命名的目录（crash/update/OTele）。只许缩小，不许扩大。
KNOWN_BLOCKED = {
    ("app_wechat_cache", r"%USERPROFILE%\Documents\WeChat Files\*\FileStorage\Cache"),
    ("app_wechat_cache", r"%USERPROFILE%\Documents\xwechat_files\*\cache"),
    ("app_wechat_cache", r"%USERPROFILE%\Documents\xwechat_files\*\temp"),
    ("app_wechat_cache", r"%APPDATA%\Tencent\xwechat\crashinfo"),
    ("app_wechat_cache", r"%APPDATA%\Tencent\xwechat\update"),
    ("app_wechat_cache", r"%APPDATA%\Tencent\WeChat\crash"),
    ("app_wechat_cache", r"%APPDATA%\Tencent\WeChat\cdncomm"),
    ("app_qq_cache", r"%USERPROFILE%\Documents\Tencent Files\*\Cache"),
    ("app_wxwork", r"%USERPROFILE%\Documents\WXWork\*\Cache"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\History"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\History-journal"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cookies"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Cookies-journal"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Top Sites"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Visited Links"),
    ("privacy_browser", r"%LOCALAPPDATA%\Google\Chrome\User Data\*\Web Data"),
    ("privacy_browser", r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\History"),
    ("privacy_browser", r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\History-journal"),
    ("privacy_browser", r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Cookies"),
    ("privacy_browser", r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Top Sites"),
    ("privacy_browser", r"%LOCALAPPDATA%\Microsoft\Edge\User Data\*\Web Data"),
    ("privacy_browser", r"%LOCALAPPDATA%\Quark\User Data\*\History"),
    ("privacy_browser", r"%LOCALAPPDATA%\Quark\User Data\*\History-journal"),
    ("privacy_browser", r"%LOCALAPPDATA%\Quark\User Data\*\Cookies"),
    ("privacy_browser", r"%LOCALAPPDATA%\Quark\User Data\*\Cookies-journal"),
    ("privacy_recent", r"%APPDATA%\Microsoft\Windows\Recent"),
    ("slim_ime", r"%SystemRoot%\IME\IMEJP"),
    ("slim_ime", r"%SystemRoot%\System32\IME\IMEJP"),
    ("slim_ime", r"%SystemRoot%\SysWOW64\IME\IMEJP"),
    ("slim_ime", r"%SystemRoot%\IME\imekr"),
    ("slim_ime", r"%SystemRoot%\System32\IME\imekr"),
    ("slim_ime", r"%SystemRoot%\SysWOW64\IME\imekr"),
    ("app_office_cache", r"%LOCALAPPDATA%\Microsoft\Office\OTele"),
}


def _expand(template):
    out = template
    for key, value in FAKE_ENV.items():
        out = out.replace("%" + key + "%", value)
    return out.replace("*", "Default")


def _shipped():
    path = os.path.join(os.path.dirname(os.path.abspath(categories.__file__)),
                        "rules.json")
    with open(path, encoding="utf-8") as fh:
        return json.load(fh)


class ShippedRulesTests(unittest.TestCase):
    def test_count_and_unique_keys(self):
        data = _shipped()
        cats = data["categories"]
        self.assertGreaterEqual(len(cats), 95)
        keys = [c["key"] for c in cats]
        self.assertEqual(len(keys), len(set(keys)))

    def test_every_rule_has_targets_and_env_paths(self):
        for cat in _shipped()["categories"]:
            targets = (cat.get("paths") or []) + (cat.get("glob_dirs") or [])
            self.assertTrue(targets or cat.get("command"), cat["key"])
            for target in targets:
                self.assertIn("%", target, f"{cat['key']}: {target}")
                self.assertFalse(safepath.is_never_clean(_expand(target)),
                                 f"{cat['key']}: {target}")
                self.assertFalse(safepath.is_password_store(_expand(target)),
                                 f"{cat['key']}: {target}")

    def test_p3_rules_pass_execution_gate(self):
        for cat in _shipped()["categories"]:
            if cat["key"] not in P3_KEYS:
                continue
            for field in ("paths", "glob_dirs"):
                for target in cat.get(field) or []:
                    self.assertTrue(
                        safepath.under_remote_allowlist(_expand(target), FAKE_ENV),
                        f"{cat['key']}: {target}")

    def test_blocked_set_does_not_grow(self):
        blocked = set()
        for cat in _shipped()["categories"]:
            if cat["key"] in P3_KEYS:
                continue
            for field in ("paths", "glob_dirs"):
                for target in cat.get(field) or []:
                    if not safepath.under_remote_allowlist(_expand(target), FAKE_ENV):
                        blocked.add((cat["key"], target))
        self.assertEqual(blocked - KNOWN_BLOCKED, set())
        self.assertTrue(blocked <= KNOWN_BLOCKED)


class LoaderEdgeTests(unittest.TestCase):
    def _load_with_document(self, document):
        with tempfile.TemporaryDirectory() as tmp:
            fake_file = os.path.join(tmp, "categories.py")
            with open(os.path.join(tmp, "rules.json"), "w", encoding="utf-8") as fh:
                json.dump(document, fh)
            with mock.patch.object(categories, "__file__", fake_file):
                return categories._load_external_rules()

    def test_keyless_and_detect_gated_rules_skipped(self):
        doc = {"categories": [
            {"name": "无 key", "glob_dirs": [r"%TEMP%\X\Cache"]},
            {"key": "gated", "name": "探测不满足",
             "glob_dirs": [r"%TEMP%\X\Cache"],
             "detect": [r"%LOCALAPPDATA%\NoSuchAppXYZ"]},
        ]}
        self.assertEqual(self._load_with_document(doc), [])

    def test_temp_cache_rule_loads_when_dir_exists(self):
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = os.path.join(tmp, "Cache")
            os.makedirs(cache_dir)
            doc = {"categories": [
                {"key": "tmp_rule", "name": "临时规则", "tier": "standard",
                 "glob_dirs": [cache_dir]},
            ]}
            with tempfile.TemporaryDirectory() as holder:
                fake_file = os.path.join(holder, "categories.py")
                with open(os.path.join(holder, "rules.json"), "w",
                          encoding="utf-8") as fh:
                    json.dump(doc, fh)
                with mock.patch.object(categories, "__file__", fake_file):
                    cats = categories._load_external_rules()
            self.assertEqual([c.key for c in cats], ["tmp_rule"])
            self.assertTrue(cats[0].untrusted)


class RemoteValidationTests(unittest.TestCase):
    BASE = {"key": "ok_rule", "kind": "files", "tier": "standard",
            "glob_dirs": [r"%LOCALAPPDATA%\App\Cache"]}

    def test_min_age_out_of_range_rejected(self):
        self.assertTrue(ruleupdate.validate_remote_rule(
            dict(self.BASE, min_age_minutes=999999)))
        self.assertTrue(ruleupdate.validate_remote_rule(
            dict(self.BASE, min_age_minutes=-1)))

    def test_exclude_overflow_rejected(self):
        self.assertTrue(ruleupdate.validate_remote_rule(
            dict(self.BASE, exclude=["*.tmp"] * 81)))

    def test_detect_and_process_fields_rejected(self):
        self.assertTrue(ruleupdate.validate_remote_rule(
            dict(self.BASE, detect=[r"%LOCALAPPDATA%\X"])))
        self.assertTrue(ruleupdate.validate_remote_rule(
            dict(self.BASE, process_absent=["a.exe"])))


class EnumerateKeysTests(unittest.TestCase):
    def test_skipped_vendors_never_expanded(self):
        opened = []

        class FakeKey:
            def __init__(self, names):
                self.names = names

            def __enter__(self):
                return self

            def __exit__(self, *exc):
                return False

        tree = {
            ("HKCU", "Software"): ["Classes", "GhostSoft"],
            ("HKCU", r"Software\GhostSoft"): ["GhostApp"],
            ("HKLM", "Software"): [],
            ("HKLM", r"Software\WOW6432Node"): [],
        }
        fake = mock.Mock()
        fake.HKEY_CURRENT_USER = "HKCU"
        fake.HKEY_LOCAL_MACHINE = "HKLM"
        fake.KEY_READ = 1

        def open_key(root, path, _r=0, _a=0):
            opened.append((root, path))
            if (root, path) not in tree:
                raise OSError("missing")
            return FakeKey(tree[(root, path)])

        def enum_key(key, index):
            if index >= len(key.names):
                raise OSError("no more")
            return key.names[index]

        fake.OpenKey.side_effect = open_key
        fake.EnumKey.side_effect = enum_key
        with mock.patch.object(leftover, "winreg", fake):
            views = leftover.enumerate_software_keys()
        self.assertNotIn(("HKCU", r"Software\Classes"), opened)
        hkcu = [v for v in views if v["hive"] == "HKCU"][0]
        self.assertEqual(hkcu["vendors"]["GhostSoft"], ["GhostApp"])
        self.assertEqual(hkcu["vendors"]["Classes"], [])


class ScannerAgeGateTests(unittest.TestCase):
    def test_single_file_root_respects_min_age(self):
        import time
        with tempfile.TemporaryDirectory() as tmp:
            fresh = os.path.join(tmp, "fresh.bin")
            with open(fresh, "wb") as fh:
                fh.write(b"x" * 10)
            cat = categories.Category(
                key="t", name="t", tier="standard", kind="files", describe="",
                paths=[fresh], min_age_minutes=1440)
            self.assertEqual(scanner.scan_category(cat), (0, 0))
            old = time.time() - 3 * 86400
            os.utime(fresh, (old, old))
            self.assertEqual(scanner.scan_category(cat), (10, 1))


class RecycleForbiddenTests(unittest.TestCase):
    def test_forbidden_root_never_sent_to_recycle(self):
        with tempfile.TemporaryDirectory() as tmp:
            cat = categories.Category(
                key="app_leftovers", name="t", tier="deep", kind="files",
                describe="", paths=[tmp], recycle=True)
            with mock.patch.object(cleaner.safepath, "execution_forbidden",
                                   return_value=True), \
                    mock.patch.object(recyclebin, "send_to_recycle") as send:
                result = cleaner.clean_category(cat)
            send.assert_not_called()
            self.assertEqual(result.skipped, 1)


class BackupSessionTests(unittest.TestCase):
    def test_session_dir_reused_within_run(self):
        with tempfile.TemporaryDirectory() as tmp:
            registry.set_backup_root(tmp)
            registry._SESSION_DIR = ""
            registry._BACKUP_FILES.clear()

            def fake_export(_root, _path, dest):
                with open(dest, "w", encoding="utf-8") as fh:
                    fh.write("reg")
                return True

            class FakeKey:
                def __enter__(self):
                    return self

                def __exit__(self, *exc):
                    return False

            fake = mock.Mock()
            fake.HKEY_CURRENT_USER = "HKCU"
            fake.HKEY_LOCAL_MACHINE = "HKLM"
            fake.KEY_READ = 1
            fake.OpenKey.return_value = FakeKey()
            try:
                with mock.patch.object(registry, "winreg", fake), \
                        mock.patch.object(registry, "_export_key",
                                          side_effect=fake_export):
                    self.assertTrue(registry.backup_hive_key(
                        "HKCU", r"Software\A\One", "One"))
                    self.assertTrue(registry.backup_hive_key(
                        "HKCU", r"Software\B\Two", "Two"))
                sessions = [d for d in os.listdir(tmp) if d.startswith("reg-")]
                self.assertEqual(len(sessions), 1)
                files = os.listdir(os.path.join(tmp, sessions[0]))
                self.assertEqual(sorted(files), ["One.reg", "Two.reg"])
                self.assertTrue(registry.backup_file_for(
                    "HKCU", r"Software\A\One").endswith("One.reg"))
            finally:
                registry._SESSION_DIR = ""
                registry._BACKUP_FILES.clear()


class PruneOrderTests(unittest.TestCase):
    def test_nested_chain_removed_root_kept(self):
        with tempfile.TemporaryDirectory() as tmp:
            deep = os.path.join(tmp, "a", "b", "c")
            os.makedirs(deep)
            removed = utils.remove_empty_dirs(tmp)
            self.assertEqual(removed, 3)
            self.assertTrue(os.path.isdir(tmp))
            self.assertFalse(os.path.exists(os.path.join(tmp, "a")))


if __name__ == "__main__":
    unittest.main()
