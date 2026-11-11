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


def find_leftovers(installed=None, roots=None, environ=None):
    """返回疑似残留目录 [{path, name}]，按名称排序。

    installed / roots 可注入，便于在没有真实注册表时测试比对逻辑。
    """
    if installed is None:
        installed = installed_software()
    if roots is None:
        roots = candidate_roots(environ)

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
