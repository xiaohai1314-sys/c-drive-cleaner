"""数据迁移：把大目录搬到其他盘，并在原位置建立目录联接（junction）。

安全策略：
1. 先完整复制到目标盘，并用整文件 SHA-256 校验。
2. 把源目录改名为备份，再建立联接并确认联接能解析。
3. 只删除清单中的文件：打开后在同一个句柄上核对大小和摘要，匹配才删除。
   清单外或对不上的文件留在备份里，结果记为部分完成。
任何一步失败都不会丢数据：目标副本在建立联接前一直保留。
"""
import ctypes
import hashlib
import os
import shutil
import stat
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


def _manifest(root: str) -> dict:
    """普通文件的相对路径 -> (大小, mtime_ns, 整文件摘要)。不进入链接。"""
    found = {}
    if not root or os.path.islink(root) or not os.path.isdir(root) or safepath.is_reparse_point(root):
        return found
    base = os.path.abspath(root)
    for entry in utils.walk_files(base):
        try:
            info = os.lstat(entry.path)
        except OSError:
            continue
        mark = _fingerprint(entry.path)
        if mark is None:
            continue
        mtime = getattr(info, "st_mtime_ns", int(info.st_mtime * 1_000_000_000))
        found[os.path.relpath(entry.path, base)] = (mark[0], mtime, mark[1])
    return found


class MigrateResult:
    """搬家结果。status 为 ok 才表示备份里已核对的文件都已删除。"""

    def __init__(self, src, dest, status="ok", backup="", remaining=()):
        self.src = src
        self.dest = dest
        self.status = status
        self.backup = backup or ""
        self.remaining = tuple(remaining)
        if status == "ok":
            self.warning = ""
        else:
            lines = ["部分完成，未校验的内容仍留在备份里。", f"备份位置：{self.backup}"]
            lines.extend(self.remaining)
            self.warning = "\n".join(lines)

    @property
    def ok(self):
        return self.status == "ok"

    def __iter__(self):
        yield self.src
        yield self.dest


def _hash_fd(fd) -> str:
    os.lseek(fd, 0, os.SEEK_SET)
    digest = hashlib.sha256()
    while True:
        block = os.read(fd, 1024 * 1024)
        if not block:
            break
        digest.update(block)
    return digest.hexdigest()


def _open_parent_dirfd(parent: str):
    """从根目录逐级 O_NOFOLLOW 打开父目录，返回 dirfd。调用方负责关闭。"""
    flags = os.O_RDONLY
    if hasattr(os, "O_DIRECTORY"):
        flags |= os.O_DIRECTORY
    if hasattr(os, "O_NOFOLLOW"):
        flags |= os.O_NOFOLLOW
    if hasattr(os, "O_CLOEXEC"):
        flags |= os.O_CLOEXEC
    absolute = os.path.abspath(parent)
    parts = [part for part in absolute.split(os.sep) if part]
    dirfd = os.open(os.sep, flags)
    try:
        for part in parts:
            nxt = os.open(part, flags, dir_fd=dirfd)
            os.close(dirfd)
            dirfd = nxt
            if not stat.S_ISDIR(os.fstat(dirfd).st_mode):
                raise OSError("父目录不是普通目录")
        owned = dirfd
        dirfd = None
        return owned
    finally:
        if dirfd is not None:
            os.close(dirfd)


def _verified_unlink(path: str, expected_size: int, expected_hash: str) -> bool:
    """打开后在同一个句柄上核对大小、身份和整文件摘要，匹配才删除。

    不匹配或打不开就留下文件。POSIX 上 unlinkat 仍按名字删除，
    最后一次核对和 unlink 之间的空隙无法在纯 Python 里关掉。
    """
    if os.name == "nt":
        return _verified_unlink_windows(path, expected_size, expected_hash)
    parent = os.path.dirname(path) or "."
    name = os.path.basename(path)
    try:
        dirfd = _open_parent_dirfd(parent)
    except OSError:
        return False
    try:
        flags = os.O_RDONLY
        if hasattr(os, "O_NOFOLLOW"):
            flags |= os.O_NOFOLLOW
        if hasattr(os, "O_CLOEXEC"):
            flags |= os.O_CLOEXEC
        try:
            fd = os.open(name, flags, dir_fd=dirfd)
        except OSError:
            return False
        try:
            info = os.fstat(fd)
            if not stat.S_ISREG(info.st_mode):
                return False
            if getattr(info, "st_file_attributes", 0) & safepath.FILE_ATTRIBUTE_REPARSE_POINT:
                return False
            if info.st_size != expected_size:
                return False
            if _hash_fd(fd) != expected_hash:
                return False
            again = os.fstat(fd)
            try:
                named = os.stat(name, dir_fd=dirfd, follow_symlinks=False)
            except OSError:
                return False
            if (again.st_dev != info.st_dev or again.st_ino != info.st_ino
                    or again.st_size != expected_size
                    or named.st_dev != info.st_dev or named.st_ino != info.st_ino):
                return False
            os.unlink(name, dir_fd=dirfd)
            return True
        finally:
            os.close(fd)
    finally:
        os.close(dirfd)


def _verified_unlink_windows(path: str, expected_size: int, expected_hash: str) -> bool:
    """不共享写权限地打开，核对句柄后用 FILE_DISPOSITION_INFO 删除。"""
    import ctypes
    from ctypes import wintypes
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.restype = ctypes.c_void_p
    kernel.CreateFileW.argtypes = [
        wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
        wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE,
    ]
    kernel.GetFileInformationByHandle.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
    kernel.GetFileInformationByHandle.restype = wintypes.BOOL
    kernel.ReadFile.argtypes = [
        wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD,
        ctypes.POINTER(wintypes.DWORD), ctypes.c_void_p,
    ]
    kernel.ReadFile.restype = wintypes.BOOL
    kernel.SetFilePointerEx.argtypes = [
        wintypes.HANDLE, ctypes.c_longlong, ctypes.c_void_p, wintypes.DWORD,
    ]
    kernel.SetFilePointerEx.restype = wintypes.BOOL
    kernel.SetFileInformationByHandle.argtypes = [
        wintypes.HANDLE, ctypes.c_uint, ctypes.c_void_p, wintypes.DWORD,
    ]
    kernel.SetFileInformationByHandle.restype = wintypes.BOOL
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    delete_access = 0x00010000
    generic_read = 0x80000000
    share_read = 0x1  # 不包含 FILE_SHARE_WRITE，哈希到删除期间拒绝别人写入
    open_existing = 3
    flag = 0x00200000  # FILE_FLAG_OPEN_REPARSE_POINT
    handle = kernel.CreateFileW(
        str(path), delete_access | generic_read, share_read, None,
        open_existing, flag, None)
    invalid = ctypes.c_void_p(-1).value
    if not handle or handle in (invalid, -1):
        return False

    class _ByHandle(ctypes.Structure):
        _fields_ = [
            ("dwFileAttributes", wintypes.DWORD),
            ("ftCreationTime", wintypes.FILETIME),
            ("ftLastAccessTime", wintypes.FILETIME),
            ("ftLastWriteTime", wintypes.FILETIME),
            ("dwVolumeSerialNumber", wintypes.DWORD),
            ("nFileSizeHigh", wintypes.DWORD),
            ("nFileSizeLow", wintypes.DWORD),
            ("nNumberOfLinks", wintypes.DWORD),
            ("nFileIndexHigh", wintypes.DWORD),
            ("nFileIndexLow", wintypes.DWORD),
        ]

    class _Disposition(ctypes.Structure):
        _fields_ = [("DeleteFile", wintypes.BOOLEAN)]

    def _identity(info):
        size = (info.nFileSizeHigh << 32) | info.nFileSizeLow
        file_id = (info.nFileIndexHigh << 32) | info.nFileIndexLow
        return info.dwFileAttributes, info.dwVolumeSerialNumber, file_id, size

    try:
        info = _ByHandle()
        if not kernel.GetFileInformationByHandle(handle, ctypes.byref(info)):
            return False
        attributes, volume, file_id, size = _identity(info)
        if attributes & safepath.FILE_ATTRIBUTE_REPARSE_POINT or size != expected_size:
            return False
        if not kernel.SetFilePointerEx(handle, 0, None, 0):
            return False
        digest = hashlib.sha256()
        if size:
            buf = ctypes.create_string_buffer(1024 * 1024)
            pending = size
            got = wintypes.DWORD(0)
            while pending > 0:
                chunk = min(len(buf), pending)
                if not kernel.ReadFile(handle, buf, chunk, ctypes.byref(got), None) or not got.value:
                    return False
                digest.update(buf.raw[:got.value])
                pending -= got.value
        if digest.hexdigest() != expected_hash:
            return False
        again = _ByHandle()
        if not kernel.GetFileInformationByHandle(handle, ctypes.byref(again)):
            return False
        attributes2, volume2, file_id2, size2 = _identity(again)
        if (attributes2 & safepath.FILE_ATTRIBUTE_REPARSE_POINT
                or volume2 != volume or file_id2 != file_id or size2 != expected_size):
            return False
        mark = _Disposition(True)
        if not kernel.SetFileInformationByHandle(handle, 4, ctypes.byref(mark), ctypes.sizeof(mark)):
            return False
        return True
    finally:
        kernel.CloseHandle(handle)


def _rmdir_if_empty(path: str) -> bool:
    """只对空目录调用 rmdir，不递归删除里面的文件。"""
    if os.path.islink(path) or safepath.is_reparse_point(path):
        return False
    try:
        os.rmdir(path)
        return True
    except OSError:
        return False


def _directories_deepest_first(root: str):
    dirs = []
    stack = [root]
    while stack:
        current = stack.pop()
        dirs.append(current)
        try:
            with os.scandir(current) as it:
                for entry in it:
                    try:
                        if entry.is_symlink():
                            continue
                        if entry.is_dir(follow_symlinks=False):
                            stack.append(entry.path)
                    except OSError:
                        continue
        except OSError:
            continue
    dirs.sort(key=len, reverse=True)
    return dirs


def _list_remaining(root: str):
    if not root or not os.path.lexists(root):
        return []
    if os.path.islink(root) or not os.path.isdir(root):
        return [root]
    found = []
    stack = [os.path.abspath(root)]
    while stack:
        current = stack.pop()
        try:
            entries = list(os.scandir(current))
        except OSError:
            continue
        for entry in entries:
            try:
                if entry.is_symlink() or not entry.is_dir(follow_symlinks=False):
                    found.append(entry.path)
                else:
                    stack.append(entry.path)
            except OSError:
                found.append(entry.path)
    return found


def _release_manifest(backup: str, manifest: dict):
    """只删除清单里、且句柄核对通过的文件。其余留下。"""
    for rel, mark in (manifest or {}).items():
        size, _mtime, digest = mark
        _verified_unlink(os.path.join(backup, rel), size, digest)
    if os.path.isdir(backup) and not os.path.islink(backup):
        for directory in _directories_deepest_first(backup):
            _rmdir_if_empty(directory)
    return _list_remaining(backup)


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
    """把 src 迁移到 dest_root 下并建立联接。返回 MigrateResult。"""
    src, dest = validate(src, dest_root)

    log(f"复制 {src}")
    log(f"  → {dest}")
    original = file_inventory(src)
    rc = _robocopy(src, dest, move=False)
    if rc >= 8 or not inventory_covers(file_inventory(dest), original):
        if inventory_covers(file_inventory(src), original) and os.path.isdir(dest):
            remove_tree(dest)
        raise MigrateError(f"复制校验失败（robocopy 返回 {rc}），源目录保持原样")

    fresh_manifest = _manifest(src)
    fresh = {rel: (size, digest) for rel, (size, _mtime, digest) in fresh_manifest.items()}
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
    remaining = _release_manifest(backup, fresh_manifest)
    if remaining:
        log(f"备份里还有未删除的内容：{backup}")
        return MigrateResult(src, dest, status="partial", backup=backup, remaining=remaining)
    log(f"已建立联接：{src} → {dest}")
    return MigrateResult(src, dest, status="ok")


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
    backup = src + ".migrate-bak"
    if os.path.lexists(backup):
        raise MigrateError(f"备份位置已存在，已停止：{backup}")
    os.rename(src, backup)
    if not _verified_unlink(backup, copied[0], copied[1]):
        return MigrateResult(
            src, dest, status="partial", backup=backup, remaining=(backup,))

    try:
        _link_file(dest, src, linker)
    except Exception as exc:
        if _restore_file(dest, src):
            raise MigrateError("无法在原位置建立链接，已还原源文件") from exc
        raise MigrateError(f"链接失败且无法还原，文件仍在：{dest}") from exc
    log(f"已搬家：{src} → {dest}")
    return MigrateResult(src, dest, status="ok")
