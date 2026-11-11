"""注册表痕迹清理：只删除已知的 MRU / 历史记录值，不碰配置项。"""
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


def group_count(group_key):
    return sum(_count(t) for t in TARGETS if t["group"] == group_key)


def group_clean(group_key):
    return sum(_delete(t) for t in TARGETS if t["group"] == group_key)
