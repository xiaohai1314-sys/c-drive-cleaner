"""注册表痕迹清理：只删除已知的 MRU / 历史记录值，不碰配置项。

删除前先把目标键导出到 logs/backups 下的 .reg 文件；导出失败就跳过该键，
不允许无备份删除。备份文件双击即可恢复。
"""
import datetime
import os
import subprocess

try:
    import winreg
except ImportError:  # 非 Windows 上保持可导入，调用时按 0 条处理
    winreg = None

HKCU = winreg.HKEY_CURRENT_USER if winreg is not None else None

TARGETS = [
    {"key": "run_mru", "group": "reg_file", "name": "运行历史",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\RunMRU", "mode": "values"},
    {"key": "typed_paths", "group": "reg_file", "name": "地址栏输入",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\TypedPaths", "mode": "values"},
    {"key": "recent_docs", "group": "reg_file", "name": "最近文档",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\RecentDocs", "mode": "values"},
    {"key": "comdlg", "group": "reg_file", "name": "打开/保存记录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\ComDlg32\OpenSavePidlMRU", "mode": "subkeys"},
    {"key": "last_visited", "group": "reg_file", "name": "最近访问目录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\ComDlg32\LastVisitedPidlMRU", "mode": "values"},
    {"key": "net_drive", "group": "reg_file", "name": "网络驱动器记录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\Map Network Drive MRU", "mode": "values"},
    {"key": "stream_mru", "group": "reg_system", "name": "文件夹视图记录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\StreamMRU", "mode": "values"},
    {"key": "mui_cache", "group": "reg_system", "name": "程序显示名缓存",
     "path": r"Software\Microsoft\Windows\ShellNoRoam\MUICache", "mode": "values"},
    {"key": "mui_cache2", "group": "reg_system", "name": "程序显示名缓存",
     "path": r"Software\Classes\Local Settings\Software\Microsoft\Windows\Shell\MuiCache", "mode": "values"},
    {"key": "recent_apps", "group": "reg_system", "name": "最近使用的应用",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Search\RecentApps", "mode": "subkeys"},
    {"key": "word_wheel", "group": "reg_search", "name": "搜索框记录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\WordWheelQuery", "mode": "values"},
    {"key": "mount_points", "group": "reg_usb", "name": "挂载/设备记录",
     "path": r"Software\Microsoft\Windows\CurrentVersion\Explorer\MountPoints2", "mode": "subkeys"},
]

GROUPS = [
    ("reg_file", "文件访问记录"),
    ("reg_system", "系统使用痕迹"),
    ("reg_search", "电脑搜索记录"),
    ("reg_usb", "USB 使用痕迹"),
]


def _count_values(key):
    count = 0
    index = 0
    while True:
        try:
            winreg.EnumValue(key, index)
            count += 1
            index += 1
        except OSError:
            return count


def _count_subkeys(key):
    count = 0
    index = 0
    while True:
        try:
            winreg.EnumKey(key, index)
            count += 1
            index += 1
        except OSError:
            return count


def _count(target):
    if winreg is None:
        return 0
    try:
        with winreg.OpenKey(HKCU, target["path"], 0, winreg.KEY_READ) as key:
            if target["mode"] == "values":
                return _count_values(key)
            return _count_subkeys(key)
    except OSError:
        return 0


def _delete_tree(parent, subkey):
    try:
        with winreg.OpenKey(parent, subkey, 0, winreg.KEY_ALL_ACCESS) as key:
            while True:
                try:
                    child = winreg.EnumKey(key, 0)
                except OSError:
                    break
                _delete_tree(key, child)
    except OSError:
        pass
    try:
        winreg.DeleteKey(parent, subkey)
    except OSError:
        pass


def _delete(target):
    deleted = 0
    if winreg is None:
        return 0
    try:
        with winreg.OpenKey(HKCU, target["path"], 0, winreg.KEY_ALL_ACCESS) as key:
            if target["mode"] == "values":
                names = []
                index = 0
                while True:
                    try:
                        name, _value, _type = winreg.EnumValue(key, index)
                        names.append(name)
                        index += 1
                    except OSError:
                        break
                for name in names:
                    try:
                        winreg.DeleteValue(key, name)
                        deleted += 1
                    except OSError:
                        pass
            else:
                subs = []
                index = 0
                while True:
                    try:
                        subs.append(winreg.EnumKey(key, index))
                        index += 1
                    except OSError:
                        break
                for sub in subs:
                    _delete_tree(key, sub)
                    deleted += 1
    except OSError:
        pass
    return deleted


# ---------- 删除前备份 ----------

_BACKUP_ROOT = None
_SESSION_DIR = None
_LAST_BACKUP = {"dir": "", "failed": []}


def set_backup_root(path):
    """由主程序把备份根目录指到与日志相同的目录下。"""
    global _BACKUP_ROOT, _SESSION_DIR
    _BACKUP_ROOT = path or None
    _SESSION_DIR = None


def _backup_root():
    if _BACKUP_ROOT:
        return _BACKUP_ROOT
    return os.path.join(os.path.dirname(os.path.abspath(__file__)),
                        "logs", "backups")


def _session_dir():
    global _SESSION_DIR
    if _SESSION_DIR:
        return _SESSION_DIR
    stamp = datetime.datetime.now().strftime("%Y%m%d-%H%M%S")
    _SESSION_DIR = os.path.join(_backup_root(), f"reg-{stamp}-{os.getpid()}")
    return _SESSION_DIR


def last_backup_info():
    """最近一次备份的目录与导出失败的键，供界面提示。"""
    return {"dir": _LAST_BACKUP["dir"], "failed": list(_LAST_BACKUP["failed"])}


def _key_exists(path):
    if winreg is None:
        return False
    try:
        with winreg.OpenKey(HKCU, path, 0, winreg.KEY_READ):
            return True
    except OSError:
        return False


def _export_key(path, dest):
    """用 reg.exe 导出 HKCU 下的一个键。成功返回 True。"""
    try:
        proc = subprocess.run(
            ["reg.exe", "export", "HKEY_CURRENT_USER\\" + path, dest, "/y"],
            capture_output=True,
            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0),
            timeout=60,
        )
    except Exception:
        return False
    return proc.returncode == 0 and os.path.isfile(dest)


def backup_key(path, label=""):
    """删除前导出一个 HKCU 键。

    键不存在视为已备份（删除本来就是空操作）；键存在但导出失败返回
    False，调用方必须跳过该键的删除。
    """
    if winreg is None:
        return True
    if not _key_exists(path):
        return True
    session = _session_dir()
    try:
        os.makedirs(session, exist_ok=True)
    except OSError:
        _LAST_BACKUP["failed"].append(path)
        return False
    name = label or path.rsplit("\\", 1)[-1] or "key"
    safe = "".join(ch if ch.isalnum() or ch in "-_" else "_" for ch in name)[:60]
    dest = os.path.join(session, safe + ".reg")
    counter = 1
    while os.path.exists(dest):
        counter += 1
        dest = os.path.join(session, f"{safe}-{counter}.reg")
    if _export_key(path, dest):
        _LAST_BACKUP["dir"] = session
        return True
    _LAST_BACKUP["failed"].append(path)
    return False


def group_count(group_key):
    return sum(_count(t) for t in TARGETS if t["group"] == group_key)


def group_count_status(group_key):
    """返回该组记录数及是否至少成功读取一个目标键。"""
    if winreg is None:
        return 0, False
    total = 0
    available = False
    for target in TARGETS:
        if target["group"] != group_key:
            continue
        try:
            with winreg.OpenKey(HKCU, target["path"], 0, winreg.KEY_READ) as key:
                available = True
                if target["mode"] == "values":
                    total += _count_values(key)
                else:
                    total += _count_subkeys(key)
        except OSError:
            continue
    return total, available


def group_clean(group_key):
    deleted = 0
    for target in TARGETS:
        if target["group"] != group_key:
            continue
        if _count(target) and not backup_key(target["path"], target["key"]):
            continue
        deleted += _delete(target)
    return deleted
