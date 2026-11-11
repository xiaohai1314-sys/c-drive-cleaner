"""数据迁移：把大目录搬到其他盘，并在原位置建立目录联接（junction）。

安全策略：
1. 先完整复制到目标盘，并用整文件 SHA-256 校验。
2. 把源目录改名为备份，再建立联接并确认联接能解析。
3. 只有联接校验通过才删除备份。任一步失败都把备份放回原路径。
任何一步失败都不会丢数据：目标副本在建立联接前一直保留。
"""
import ctypes
import hashlib
import os
import shutil
import subprocess

import safepath
import utils

FILE_ATTRIBUTE_REPARSE_POINT = 0x400
CREATE_NO_WINDOW = 0x08000000


class MigrateError(Exception):
    pass


def is_junction(path: str) -> bool:
    try:
        attrs = ctypes.windll.kernel32.GetFileAttributesW(path)
    except Exception:
        return False
    if attrs is None or attrs == -1:
        return False
    return bool(attrs & FILE_ATTRIBUTE_REPARSE_POINT)


def junction_target(path: str) -> str:
    """返回联接/符号链接指向的真实路径，失败返回空串。"""
    try:
        return os.path.realpath(path)
    except Exception:
        return ""


def count_files(root: str) -> int:
    return sum(1 for _ in utils.walk_files(root))


def dir_size(root: str) -> int:
    total = 0
    for entry in utils.walk_files(root):
        try:
            total += entry.stat(follow_symlinks=False).st_size
        except OSError:
            pass
    return total


def _protected_roots():
    names = ["SystemRoot", "ProgramFiles", "ProgramFiles(x86)",
             "ProgramData", "USERPROFILE", "LOCALAPPDATA", "APPDATA"]
    roots = []
    for name in names:
        value = os.environ.get(name)
        if value:
            roots.append(os.path.normcase(os.path.abspath(value)).rstrip("\\"))
    return roots


def _drive_of(path: str) -> str:
    return os.path.splitdrive(os.path.abspath(path))[0].upper()


def validate(src: str, dest_root: str):
    """校验并返回 (src, dest)。不合法时抛出 MigrateError。"""
    src = os.path.abspath(src)
    dest_root = os.path.abspath(dest_root)

    if not os.path.isdir(src):
        raise MigrateError("源目录不存在")
    if is_junction(src):
        raise MigrateError("该目录已经是联接，无需迁移")
    if len(src.rstrip("\\")) <= 2:
        raise MigrateError("不能迁移整个磁盘根目录")

    norm_src = os.path.normcase(src).rstrip("\\")
    if norm_src in _protected_roots():
        raise MigrateError("该目录属于系统或用户配置根目录，不能迁移")

    if not _drive_of(dest_root):
        raise MigrateError("目标位置需要指定盘符")
    if _drive_of(src) == _drive_of(dest_root):
        raise MigrateError("目标盘必须与源目录所在盘不同")

    name = os.path.basename(src.rstrip("\\"))
    if not name:
        raise MigrateError("无法确定目录名")
    dest = os.path.join(dest_root, name)
    if os.path.exists(dest):
        raise MigrateError(f"目标已存在：{dest}")
    return src, dest


def _robocopy(src: str, dst: str, move: bool) -> int:
    args = ["robocopy", src, dst, "/E", "/R:1", "/W:1",
            "/NFL", "/NDL", "/NJH", "/NJS", "/NP"]
    if move:
        args.append("/MOVE")
    try:
        proc = subprocess.run(args, capture_output=True,
                              creationflags=CREATE_NO_WINDOW)
    except Exception as exc:
        raise MigrateError(f"无法运行 robocopy：{exc}")
    return proc.returncode


def _remove_junction_dirs(root: str):
    """删除 root 之下（不含 root）的空目录与联接，跳过普通非空目录。"""
    stack = [root]
    dirs = []
    while stack:
        current = stack.pop()
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_dir(follow_symlinks=False):
                            if utils.is_link_like(entry):
                                try:
                                    os.rmdir(entry.path)  # 只删联接本身
                                except OSError:
                                    pass
                            else:
                                dirs.append(entry.path)
                                stack.append(entry.path)
                    except OSError:
                        continue
        except OSError:
            continue
    for path in sorted(dirs, key=len, reverse=True):
        try:
            os.rmdir(path)
        except OSError:
            pass


def remove_tree(root: str):
    """安全删除目录：先删文件，再删空目录，绝不跟随联接。"""
    if is_junction(root):
        try:
            os.rmdir(root)
        except OSError:
            pass
        return
    for entry in utils.walk_files(root):
        os.remove(entry.path)
    _remove_junction_dirs(root)
    try:
        os.rmdir(root)
    except OSError:
        pass


def create_junction(link: str, target: str):
    try:
        proc = subprocess.run(["cmd", "/c", "mklink", "/J", link, target],
                              capture_output=True, creationflags=CREATE_NO_WINDOW)
    except Exception as exc:
        raise MigrateError(f"无法运行 mklink：{exc}")
    if proc.returncode != 0 or not is_junction(link):
        message = (proc.stderr or proc.stdout or b"").decode("mbcs", "replace").strip()
        raise MigrateError(f"创建联接失败：{message or '未知错误'}")


def junction_works(link: str, target: str) -> bool:
    """校验联接不仅能列出根目录，还能真正进入子目录。

    某些环境（如安全软件的文件过滤驱动）下，跨卷联接在 AppData 等位置
    会返回 ERROR_MOUNT_POINT_NOT_RESOLVED，根目录可列出但子目录不可访问。
    这里以目标盘的真实内容为参照，从联接路径进入同一子目录来验证。
    """
    try:
        target_items = os.listdir(target)
    except OSError:
        return False
    if not target_items:
        return True
    for name in target_items:
        if os.path.isdir(os.path.join(target, name)):
            try:
                os.listdir(os.path.join(link, name))
            except OSError:
                return False
            return True
    name = target_items[0]
    return os.path.exists(os.path.join(link, name))


def _fingerprint(path: str):
    """文件的 (大小, 摘要)。链接返回 None，避免把链接目标当成副本。"""
    try:
        info = os.lstat(path)
    except OSError:
        return None
    if safepath.is_reparse_point(path):
        return None
    size = info.st_size
    digest = hashlib.sha256()
    try:
        with open(path, "rb") as handle:
            while True:
                block = handle.read(1024 * 1024)
                if not block:
                    break
                digest.update(block)
    except OSError:
        return None
    return (size, digest.hexdigest())


def file_inventory(root: str) -> dict:
    """root 下普通文件的相对路径指纹。不进入链接。"""
    found = {}
    if not root or not os.path.isdir(root) or safepath.is_reparse_point(root):
        return found
    base = os.path.abspath(root)
    for entry in utils.walk_files(base):
        mark = _fingerprint(entry.path)
        if mark is None:
            continue
        found[os.path.relpath(entry.path, base)] = mark
    return found


def inventory_covers(have: dict, need: dict) -> bool:
    """have 是否包含 need 里的每一份，且大小和摘要都相同。"""
    for rel, mark in (need or {}).items():
        if not mark or have.get(rel) != mark:
            return False
    return True


def _rollback_to_source(src: str, dest: str):
    """把目标副本拷回源位置。校验通过后才删除目标，避免删掉唯一的一份。"""
    if safepath.is_reparse_point(src):
        try:
            os.rmdir(src)
        except OSError as exc:
            raise MigrateError(f"无法移除联接，目标副本已保留：{exc}")
    os.makedirs(src, exist_ok=True)
    needed = file_inventory(dest)
    _robocopy(dest, src, move=False)
    if not inventory_covers(file_inventory(src), needed):
        raise MigrateError("还原校验失败，目标位置的副本已保留，没有删除唯一的一份")
    if os.path.isdir(dest):
        remove_tree(dest)


def migrate(src: str, dest_root: str, log=lambda _message: None):
    """把 src 迁移到 dest_root 下并建立联接。返回 (src, dest)。"""
    src, dest = validate(src, dest_root)

    log(f"复制 {src}")
    log(f"  → {dest}")
    original = file_inventory(src)
    rc = _robocopy(src, dest, move=False)
    if rc >= 8 or not inventory_covers(file_inventory(dest), original):
        if inventory_covers(file_inventory(src), original) and os.path.isdir(dest):
            remove_tree(dest)
        raise MigrateError(f"复制校验失败（robocopy 返回 {rc}），源目录保持原样")

    fresh = file_inventory(src)
    if fresh != original or not inventory_covers(file_inventory(dest), fresh):
        raise MigrateError("源目录在复制后发生变化，源文件和目标副本都已保留")

    backup = src.rstrip("\\/") + ".migrate-bak"
    if os.path.lexists(backup):
        raise MigrateError(f"备份位置已存在，已停止：{backup}")
    os.rename(src, backup)
    try:
        create_junction(src, dest)
        if not junction_works(src, dest):
            raise MigrateError("联接校验失败")
    except Exception as exc:
        log("建立联接失败，正在把源目录还原…")
        _restore_renamed_source(src, backup)
        if inventory_covers(file_inventory(src), original) and os.path.isdir(dest):
            remove_tree(dest)
        raise MigrateError(
            "建立联接失败，源目录已还原，数据未丢失") from exc
    remove_tree(backup)
    log(f"已建立联接：{src} → {dest}")
    return src, dest


def _restore_renamed_source(src: str, backup: str):
    """联接失败时把改名的源目录放回原路径。"""
    if not os.path.lexists(backup):
        raise MigrateError(f"找不到备份，无法还原：{backup}")
    if os.path.lexists(src):
        if os.path.islink(src) or safepath.is_reparse_point(src):
            try:
                os.unlink(src)
            except OSError:
                os.rmdir(src)
        elif os.path.isdir(src) and not os.listdir(src):
            os.rmdir(src)
        else:
            raise MigrateError(f"原路径被占用，数据仍在 {backup}")
    os.rename(backup, src)


def undo(src: str, log=lambda _message: None):
    """撤销迁移：删除联接并把数据搬回原位置。"""
    src = os.path.abspath(src)
    if not is_junction(src):
        raise MigrateError("该目录不是联接，无法撤销")
    target = junction_target(src)
    if not target or not os.path.isdir(target):
        raise MigrateError("找不到联接指向的目录，无法撤销")

    log(f"移除联接 {src}")
    try:
        os.rmdir(src)
    except OSError as exc:
        raise MigrateError(f"移除联接失败：{exc}")

    log(f"搬回 {target}")
    log(f"  → {src}")
    needed = file_inventory(target)
    rc = _robocopy(target, src, move=False)
    if rc >= 8 or not inventory_covers(file_inventory(src), needed):
        raise MigrateError(
            "搬回校验失败，目标副本已保留，没有删除唯一的一份。"
            f"请检查原位置 {src} 和目标 {target}")

    remove_tree(target)
    log(f"已还原：{src}")
    return src


_UNINSTALL_KEYS = (
    ("HKLM", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
    ("HKLM", r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall"),
    ("HKCU", r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall"),
)


def _open_registry():
    try:
        import winreg
    except ImportError:
        return None
    return winreg


def _enum_values(reg, key):
    values = {}
    index = 0
    while True:
        try:
            name, data, _typ = reg.EnumValue(key, index)
        except OSError:
            break
        values[name] = data
        index += 1
    return values


def _first_exe(text):
    text = str(text or "").strip()
    if not text:
        return ""
    if text.startswith('"'):
        end = text.find('"', 1)
        if end > 1:
            return text[1:end]
    return text.split(" ")[0].strip('"')


def _location_from_values(values, exists):
    location = str(values.get("InstallLocation") or "").strip().strip('"')
    if location and exists(location):
        return location
    icon = str(values.get("DisplayIcon") or "").strip().strip('"')
    if icon:
        icon = icon.split(",")[0].strip().strip('"')
        folder = os.path.dirname(icon)
        if folder and exists(folder):
            return folder
    exe = _first_exe(values.get("UninstallString"))
    folder = os.path.dirname(exe) if exe else ""
    if folder and exists(folder):
        return folder
    return ""


def discover_installed(reg=None, environ=None, exists=None):
    """从卸载注册表发现已安装软件。不提供文档、桌面、下载和系统目录。"""
    environ = os.environ if environ is None else environ
    exists = os.path.isdir if exists is None else exists
    if reg is None:
        reg = _open_registry()
    if reg is None:
        return []
    hives = {
        "HKLM": reg.HKEY_LOCAL_MACHINE,
        "HKCU": reg.HKEY_CURRENT_USER,
    }
    seen = set()
    found = []
    for hive_name, subpath in _UNINSTALL_KEYS:
        try:
            root = reg.OpenKey(hives[hive_name], subpath)
        except OSError:
            continue
        index = 0
        while True:
            try:
                name = reg.EnumKey(root, index)
            except OSError:
                break
            index += 1
            try:
                sub = reg.OpenKey(root, name)
            except OSError:
                continue
            values = _enum_values(reg, sub)
            closer = getattr(sub, "Close", None)
            if closer:
                closer()
            display = str(values.get("DisplayName") or "").strip()
            if not display:
                continue
            if str(values.get("SystemComponent") or "") in ("1", "0x1"):
                continue
            try:
                if int(values.get("SystemComponent") or 0) == 1:
                    continue
            except (TypeError, ValueError):
                pass
            if values.get("ReleaseType") or values.get("ParentKeyName"):
                continue
            location = _location_from_values(values, exists)
            if not location:
                continue
            if safepath.migration_forbidden(location, environ):
                continue
            marker = safepath.normalize(location).lower()
            if marker in seen:
                continue
            seen.add(marker)
            try:
                size = int(values.get("EstimatedSize") or 0) * 1024
            except (TypeError, ValueError):
                size = 0
            found.append({
                "name": display,
                "path": location,
                "size": max(size, 0),
                "publisher": str(values.get("Publisher") or ""),
            })
        closer = getattr(root, "Close", None)
        if closer:
            closer()
    found.sort(key=lambda item: item["size"], reverse=True)
    return found


def candidate_folders():
    """软件搬家候选：只来自卸载注册表，不含文档/桌面/下载。"""
    return [item["path"] for item in discover_installed()]


def _same_file_size(src: str, dest: str) -> bool:
    try:
        return os.path.getsize(src) == os.path.getsize(dest)
    except OSError:
        return False


def _link_file(dest: str, src: str, linker=None):
    if linker is not None:
        linker(dest, src)
        return
    os.symlink(dest, src)


def _restore_file(dest: str, src: str) -> bool:
    """链接失败时把文件拷回原处并删掉目标副本。"""
    try:
        if os.path.lexists(src):
            os.remove(src)
        shutil.copy2(dest, src)
        if _same_file_size(dest, src):
            os.remove(dest)
            return True
    except OSError:
        return False
    return False


def migrate_file(src: str, dest_root: str, log=lambda _message: None, linker=None):
    """把单个大文件搬到其他盘，并在原路径留下符号链接。

    复制校验失败时源文件不动。链接建不起来时把文件拷回原处。
    """
    src = os.path.abspath(src)
    dest_root = os.path.abspath(dest_root)
    if safepath.is_system_path(src):
        raise MigrateError("系统文件不能搬家")
    if not os.path.isfile(src):
        raise MigrateError("源文件不存在")
    if not _drive_of(dest_root):
        raise MigrateError("目标位置需要指定盘符")
    if _drive_of(src) == _drive_of(dest_root):
        raise MigrateError("目标盘必须与源文件所在盘不同")
    os.makedirs(dest_root, exist_ok=True)
    dest = os.path.join(dest_root, os.path.basename(src))
    if os.path.exists(dest):
        raise MigrateError(f"目标已存在：{dest}")

    log(f"复制 {src}")
    log(f"  → {dest}")
    before = _fingerprint(src)
    shutil.copy2(src, dest)
    source_now = _fingerprint(src)
    copied = _fingerprint(dest)
    if source_now != before:
        raise MigrateError("源文件在复制后已变化，源文件和目标副本都已保留")
    if not _same_file_size(src, dest) or copied is None or copied != source_now:
        try:
            os.remove(dest)
        except OSError:
            pass
        raise MigrateError("复制校验失败，已取消，源文件未改动")

    again = _fingerprint(src)
    if again != copied:
        raise MigrateError("源文件在删除前已变化，源文件和目标副本都已保留")
    try:
        os.remove(src)
    except OSError as exc:
        try:
            os.remove(dest)
        except OSError:
            pass
        raise MigrateError(f"无法移走源文件，已取消：{exc}")

    try:
        _link_file(dest, src, linker)
    except Exception as exc:
        if _restore_file(dest, src):
            raise MigrateError("无法在原位置建立链接，已还原源文件") from exc
        raise MigrateError(f"链接失败且无法还原，文件仍在：{dest}") from exc
    log(f"已搬家：{src} → {dest}")
    return src, dest
