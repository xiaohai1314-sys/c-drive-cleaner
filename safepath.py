"""路径安全判断：系统目录保护、用户文档库、浏览器密码库。

Windows 盘符路径在 Linux 上不能交给 os.path.abspath（会被拼到当前目录）。
这里对「盘符:」开头的路径按 Windows 规则规范化，其余路径仍走本机绝对路径。
"""
import os
import sys


PASSWORD_FILENAMES = {
    "login data",
    "login data-journal",
    "login data for account",
    "login data for account-journal",
    "logins.json",
    "logins-backup.json",
    "key4.db",
    "key3.db",
}

_CRITICAL_FILES = {
    "pagefile.sys", "hiberfil.sys", "swapfile.sys", "bootmgr", "ntldr",
    "ntuser.dat", "ntuser.dat.log",
}

# 紧跟盘符的系统目录。比较时要求目录边界，避免把 Windows.old 误判成 Windows。
_SYSTEM_ROOTS = (
    "\\windows",
    "\\program files",
    "\\program files (x86)",
    "\\programdata",
    "\\system volume information",
    "\\$recycle.bin",
    "\\recovery",
    "\\boot",
    "\\perflogs",
)

_LIBRARY_NAMES = (
    "documents", "desktop", "downloads",
    "my documents", "文档", "桌面", "下载",
    "pictures", "music", "videos",
)


def basename(path: str) -> str:
    """同时认识 Windows 反斜杠和本机分隔符。"""
    text = str(path or "").replace("/", "\\").rstrip("\\")
    if "\\" in text:
        return text.rsplit("\\", 1)[-1]
    return os.path.basename(text)


def normalize(path: str) -> str:
    """返回去掉末尾分隔符的规范路径。盘符路径保持 Windows 形态。"""
    if not path:
        return ""
    text = str(path).strip().strip('"').replace("/", "\\")
    if len(text) >= 2 and text[1] == ":":
        drive = text[:2].upper()
        rest = text[2:].replace("\\\\", "\\")
        while "\\\\" in rest:
            rest = rest.replace("\\\\", "\\")
        if rest in ("", "\\"):
            return drive + "\\"
        if not rest.startswith("\\"):
            rest = "\\" + rest
        return (drive + rest).rstrip("\\")
    return os.path.normcase(os.path.abspath(text)).rstrip("\\/")


def _tail(path: str) -> str:
    """盘符之后的部分，统一成小写且以反斜杠开头。"""
    norm = normalize(path)
    if len(norm) >= 2 and norm[1] == ":":
        rest = norm[2:]
    else:
        rest = norm
    rest = rest.replace("/", "\\").lower()
    if not rest.startswith("\\"):
        rest = "\\" + rest
    return rest


def _is_under(tail: str, marker: str) -> bool:
    marker = marker.lower()
    if not marker.startswith("\\"):
        marker = "\\" + marker
    return tail == marker or tail.startswith(marker + "\\")


def _env_roots():
    roots = []
    for name in ("SystemRoot", "WINDIR", "ProgramFiles", "ProgramFiles(x86)", "ProgramData"):
        value = os.environ.get(name)
        if value:
            roots.append(normalize(value).lower())
    return roots


def is_drive_root(path: str) -> bool:
    norm = normalize(path)
    if len(norm) == 2 and norm[1] == ":":
        return True
    if len(norm) == 3 and norm[1] == ":" and norm[2] in "\\/":
        return True
    return False


def is_password_store(path: str) -> bool:
    """浏览器保存的密码库。隐私清理默认不能删这些文件。"""
    return basename(path).lower() in PASSWORD_FILENAMES


def is_critical_file(path: str) -> bool:
    return basename(path).lower() in _CRITICAL_FILES


def _under_env_root(path: str) -> bool:
    target = normalize(path).lower().rstrip("\\")
    for root in _env_roots():
        root = root.rstrip("\\")
        if not root:
            continue
        if target == root or target.startswith(root + "\\"):
            return True
    return False


def _under_marker(path: str, markers) -> bool:
    tail = _tail(path)
    return any(_is_under(tail, marker) for marker in markers)


def is_system_path(path: str) -> bool:
    """Windows 目录、盘符根、关键系统文件、卷信息。粉碎和搬家都不能碰。"""
    if not path or not str(path).strip():
        return True
    if is_drive_root(path) or is_critical_file(path):
        return True
    if _under_marker(path, ("\\windows", "\\system volume information",
                            "\\recovery", "\\boot", "\\$recycle.bin")):
        return True
    system = os.environ.get("SystemRoot") or os.environ.get("WINDIR")
    if system:
        root = normalize(system).lower().rstrip("\\")
        target = normalize(path).lower().rstrip("\\")
        if target == root or target.startswith(root + "\\"):
            return True
    return False


def is_protected_path(path: str) -> bool:
    """粉碎保护：系统目录、Program Files、ProgramData、用户配置根本身、本程序。"""
    if is_system_path(path):
        return True
    if _under_marker(path, ("\\program files", "\\program files (x86)", "\\programdata",
                            "\\perflogs")):
        return True
    if _under_env_root(path):
        return True
    profile = os.environ.get("USERPROFILE")
    if profile and normalize(path).lower().rstrip("\\") == normalize(profile).lower().rstrip("\\"):
        return True
    exe = getattr(sys, "executable", "") or ""
    if exe:
        home = normalize(os.path.dirname(exe)).lower().rstrip("\\")
        target = normalize(path).lower().rstrip("\\")
        # 只在解释器位于受保护目录时才连坐，避免挡住用户目录里的普通文件。
        if home and (is_system_path(home) or _under_marker(home, ("\\program files",
                     "\\program files (x86)", "\\programdata"))):
            if target == home or target.startswith(home + "\\"):
                return True
    return False


def user_library_roots(environ=None):
    env = environ if environ is not None else os.environ
    roots = []
    bases = []
    for key in ("USERPROFILE", "HOME"):
        value = env.get(key)
        if value:
            bases.append(value)
    one = env.get("OneDrive") or env.get("OneDriveConsumer")
    if one:
        bases.append(one)
    for base in bases:
        for name in ("Documents", "Desktop", "Downloads", "My Documents",
                     "文档", "桌面", "下载", "Pictures", "Music", "Videos"):
            roots.append(normalize(os.path.join(base, name)))
    return roots


def is_user_library(path: str, environ=None) -> bool:
    """文档、桌面、下载及其子路径。软件搬家不能提供这些目录。"""
    if not path:
        return False
    target = normalize(path).lower().rstrip("\\")
    for root in user_library_roots(environ):
        root = root.lower().rstrip("\\")
        if target == root or target.startswith(root + "\\"):
            return True
    # 环境变量缺失时，仍识别「用户目录下的已知库文件夹」。
    parts = [part for part in _tail(path).split("\\") if part]
    if len(parts) >= 3 and parts[0] == "users" and parts[2] in _LIBRARY_NAMES:
        return True
    if len(parts) >= 2 and parts[0] == "users" and parts[1] in _LIBRARY_NAMES:
        return True
    return False


def migration_forbidden(path: str, environ=None) -> bool:
    """软件搬家候选：拒绝系统根、整个 Program Files、以及文档/桌面/下载。

    Program Files 里的单个软件目录允许搬家；目录本身不允许。
    """
    if not path or is_system_path(path) or is_user_library(path, environ):
        return True
    tail = _tail(path)
    exact = {
        "\\program files", "\\program files (x86)", "\\programdata", "\\users",
    }
    if tail in exact:
        return True
    profile = (environ or os.environ).get("USERPROFILE")
    if profile and normalize(path).lower().rstrip("\\") == normalize(profile).lower().rstrip("\\"):
        return True
    return False
