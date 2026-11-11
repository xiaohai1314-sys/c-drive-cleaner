"""卸载残留：拿卸载注册表反查一级目录，列出找不到对应软件的孤儿目录。

判定只用名称/发布者/安装位置比对，宁可漏报不猜：便携软件、手动解压的
软件也会被列进来，所以结果是"疑似残留"，对应清理项固定深度档 +
需确认 + 默认不勾选（由 categories 侧保证）。
"""
import os
import re

try:
    import winreg
except ImportError:  # 非 Windows 上保持可导入，枚举结果为空
    winreg = None

_UNINSTALL_SUBKEY = r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
_UNINSTALL_SUBKEY_WOW = r"Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"

# 系统与公共目录，不作为残留候选（小写比较）。
_SYSTEM_DIR_NAMES = {
    "microsoft", "windows", "common files", "internet explorer",
    "windows defender", "windows nt", "windows media player",
    "windows photo viewer", "reference assemblies", "package cache",
    "msbuild", "windows kits", "windowsapps", "application verifier",
    "windows mail", "windows sidebar", "dvd maker", "assembly",
    "temp", "temporary internet files",
}


def _norm(text):
    """归一化名称：小写，只留字母/数字/汉字，用于模糊比对。"""
    return re.sub(r"[^0-9a-z\u4e00-\u9fff]+", "", str(text or "").lower())


def _read_value(key, name):
    try:
        value, _kind = winreg.QueryValueEx(key, name)
    except OSError:
        return ""
    return str(value or "").strip()


def installed_software():
    """枚举卸载注册表，返回 [{name, publisher, install_location}]。"""
    if winreg is None:
        return []
    found = []
    seen = set()
    sources = [
        (winreg.HKEY_LOCAL_MACHINE, _UNINSTALL_SUBKEY),
        (winreg.HKEY_LOCAL_MACHINE, _UNINSTALL_SUBKEY_WOW),
        (winreg.HKEY_CURRENT_USER, _UNINSTALL_SUBKEY),
    ]
    for root, base in sources:
        try:
            with winreg.OpenKey(root, base, 0, winreg.KEY_READ) as parent:
                index = 0
                while True:
                    try:
                        sub = winreg.EnumKey(parent, index)
                    except OSError:
                        break
                    index += 1
                    try:
                        with winreg.OpenKey(parent, sub, 0, winreg.KEY_READ) as item:
                            name = _read_value(item, "DisplayName")
                            if not name:
                                continue
                            entry = {
                                "name": name,
                                "publisher": _read_value(item, "Publisher"),
                                "install_location": _read_value(item, "InstallLocation"),
                            }
                    except OSError:
                        continue
                    marker = (entry["name"].lower(), entry["install_location"].lower())
                    if marker in seen:
                        continue
                    seen.add(marker)
                    found.append(entry)
        except OSError:
            continue
    return found


def candidate_roots(environ=None):
    env = environ if environ is not None else os.environ
    roots = []
    for key in ("APPDATA", "LOCALAPPDATA", "PROGRAMDATA",
                "ProgramFiles", "ProgramFiles(x86)"):
        value = env.get(key)
        if value and os.path.isdir(value) and value not in roots:
            roots.append(value)
    return roots


def _norm_path(path):
    return os.path.normcase(os.path.normpath(str(path or ""))).rstrip("\\/")


def _within(path, roots):
    target = _norm_path(path)
    for root in roots:
        base = _norm_path(root)
        if base and (target == base or target.startswith(base + "\\")
                     or target.startswith(base + "/")):
            return True
    return False


def _name_known(token, known):
    if token in known:
        return True
    for other in known:
        if len(token) >= 3 and len(other) >= 3 and (token in other or other in token):
            return True
    return False


def _known_tokens(installed):
    """从已安装软件里提取可识别标记：名称、发布者、安装目录名（归一化）。"""
    known = set()
    locations = []
    for app in installed or []:
        for value in (app.get("name"), app.get("publisher")):
            token = _norm(value)
            if len(token) >= 2:
                known.add(token)
        location = str(app.get("install_location") or "").strip().strip('"')
        if location:
            locations.append(location)
            token = _norm(os.path.basename(location.rstrip("\\/")))
            if len(token) >= 2:
                known.add(token)
    return known, locations


def find_leftovers(installed=None, roots=None, environ=None):
    """返回疑似残留目录 [{path, name}]，按名称排序。

    installed / roots 可注入，便于在没有真实注册表时测试比对逻辑。
    """
    if installed is None:
        installed = installed_software()
    if roots is None:
        roots = candidate_roots(environ)

    known, locations = _known_tokens(installed)

    leftovers = []
    seen = set()
    for root in roots:
        try:
            names = os.listdir(root)
        except OSError:
            continue
        for name in names:
            path = os.path.join(root, name)
            marker = _norm_path(path)
            if marker in seen:
                continue
            seen.add(marker)
            try:
                if not os.path.isdir(path) or os.path.islink(path):
                    continue
            except OSError:
                continue
            if name.lower() in _SYSTEM_DIR_NAMES or name.startswith("."):
                continue
            token = _norm(name)
            if not token:
                continue
            # 目录本身或其子目录是某软件的安装位置，或它包含安装位置。
            if _within(path, locations) or _within_any_location_inside(path, locations):
                continue
            if _name_known(token, known):
                continue
            leftovers.append({"path": path, "name": name})
    leftovers.sort(key=lambda item: item["name"].lower())
    return leftovers


def _within_any_location_inside(path, locations):
    """path 是某个安装位置的父目录时不算残留（如 Program Files\\Google）。"""
    base = _norm_path(path)
    for location in locations:
        target = _norm_path(location)
        if target.startswith(base + "\\") or target.startswith(base + "/"):
            return True
    return False


# ---------- 注册表残留 ----------

# Software 一级键里永不判定为残留的系统与共享区域（小写）。
# Microsoft 名下集中了系统组件、.NET / VC++ 运行库与驱动基础设施；
# Classes 是类注册区，枚举它既慢又危险，直接跳过不打开。
_SKIP_SOFTWARE_KEYS = {
    "microsoft", "windows", "classes", "policies", "registeredapplications",
    "clients", "odbc", "wow6432node", "intel", "common files",
}

# 厂商键下没有软件归属含义的子键名（小写）：功能性子键（遥测、日志、
# 诊断等）即使厂商软件已卸载也不该按“某款软件的残留”处理。
_SKIP_APP_NAMES = {
    "common", "shared", "update", "updater", "settings", "config",
    "components", "registration", "currentversion", "data", "plugins",
    "telemetry", "diagnostics", "diagnostic", "crashpad", "logs", "log",
    "metrics", "usage", "feedback", "traces",
}

_MAX_VENDORS = 512
_MAX_APPS_PER_VENDOR = 256


def _enum_subkeys(root, path):
    names = []
    if winreg is None:
        return names
    try:
        with winreg.OpenKey(root, path, 0, winreg.KEY_READ) as key:
            index = 0
            while True:
                try:
                    names.append(winreg.EnumKey(key, index))
                except OSError:
                    break
                index += 1
    except OSError:
        pass
    return names


def enumerate_software_keys():
    """枚举 Software 区域的一级（厂商）与二级（应用）键名。

    返回 [{hive, prefix, vendors: {厂商: [应用, ...]}}]。
    _SKIP_SOFTWARE_KEYS 里的厂商不展开子键（Classes 尤其不能展开）。
    """
    if winreg is None:
        return []
    views = []
    for hive, root, prefix in (
        ("HKCU", winreg.HKEY_CURRENT_USER, "Software"),
        ("HKLM", winreg.HKEY_LOCAL_MACHINE, "Software"),
        ("HKLM", winreg.HKEY_LOCAL_MACHINE, r"Software\WOW6432Node"),
    ):
        vendors = {}
        for vendor in _enum_subkeys(root, prefix)[:_MAX_VENDORS]:
            if vendor.lower() in _SKIP_SOFTWARE_KEYS:
                vendors[vendor] = []
                continue
            vendors[vendor] = _enum_subkeys(
                root, prefix + "\\" + vendor)[:_MAX_APPS_PER_VENDOR]
        views.append({"hive": hive, "prefix": prefix, "vendors": vendors})
    return views


def match_registry_leftovers(vendor_views, installed, dir_tokens):
    """纯比对：返回疑似注册表残留 [{hive, path, name}]。

    判定条件（同时满足）：应用/厂商标记未出现在已安装软件里；
    未出现在文件侧一级目录里（便携软件还活着就不算残留）；
    不在系统与共享区域名单里。
    """
    known, _locations = _known_tokens(installed)
    dir_tokens = dir_tokens or set()
    out = []
    seen = set()
    for view in vendor_views or []:
        hive = view.get("hive")
        prefix = view.get("prefix") or "Software"
        for vendor, apps in (view.get("vendors") or {}).items():
            if vendor.lower() in _SKIP_SOFTWARE_KEYS:
                continue
            vtoken = _norm(vendor)
            if not vtoken:
                continue
            if not apps:
                # 一级键本身就是应用键（无子键）。
                if _name_known(vtoken, known) or _name_known(vtoken, dir_tokens):
                    continue
                path = prefix + "\\" + vendor
                if (hive, path) not in seen:
                    seen.add((hive, path))
                    out.append({"hive": hive, "path": path, "name": vendor})
                continue
            for app in apps:
                if app.lower() in _SKIP_APP_NAMES:
                    continue
                atoken = _norm(app)
                if not atoken:
                    continue
                if _name_known(atoken, known) or _name_known(atoken, dir_tokens):
                    continue
                path = prefix + "\\" + vendor + "\\" + app
                if (hive, path) in seen:
                    continue
                seen.add((hive, path))
                out.append({"hive": hive, "path": path, "name": app})
    out.sort(key=lambda item: (item["hive"], item["path"].lower()))
    return out


def _dir_tokens(roots):
    tokens = set()
    for root in roots or []:
        try:
            names = os.listdir(root)
        except OSError:
            continue
        for name in names:
            token = _norm(name)
            if len(token) >= 2:
                tokens.add(token)
    return tokens


def find_registry_leftovers(installed=None, roots=None, environ=None):
    """枚举真实注册表并比对，返回疑似残留键 [{hive, path, name}]。"""
    if installed is None:
        installed = installed_software()
    if roots is None:
        roots = candidate_roots(environ)
    return match_registry_leftovers(
        enumerate_software_keys(), installed, _dir_tokens(roots))
