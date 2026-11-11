"""数据迁移：把大目录搬到其他盘，并在原位置建立目录联接（junction）。

安全策略：
1. 先完整复制到目标盘（robocopy /MOVE 在复制成功后才删除源文件）。
2. 复制后校验源目录是否还有残留文件；有残留（通常是文件被占用）就
   从目标盘把已删除的文件复制回源目录，再删除目标副本，保持原状。
3. 只有确认源目录已清空，才删除空目录并建立联接。
任何一步失败都不会丢数据：目标副本在建立联接前一直保留。
"""
import ctypes
import os
import subprocess

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


def _rollback_to_source(src: str, dest: str):
    """把数据从目标搬回源位置并清理目标。"""
    try:
        os.rmdir(src)
    except OSError:
        pass
    _robocopy(dest, src, move=True)
    if os.path.isdir(dest):
        remove_tree(dest)


def migrate(src: str, dest_root: str, log=lambda _message: None):
    """把 src 迁移到 dest_root 下并建立联接。返回 (src, dest)。"""
    src, dest = validate(src, dest_root)

    log(f"复制 {src}")
    log(f"  → {dest}")
    rc = _robocopy(src, dest, move=True)
    if rc >= 8:
        if os.path.isdir(dest):
            remove_tree(dest)
        raise MigrateError(f"复制失败（robocopy 返回 {rc}），未改动源目录")

    remaining = count_files(src)
    if remaining:
        log(f"源目录仍有 {remaining} 个文件被占用，正在还原…")
        _robocopy(dest, src, move=False)
        if os.path.isdir(dest):
            remove_tree(dest)
        raise MigrateError(
            f"{remaining} 个文件被占用无法移动，已全部还原。"
            "请先完全关闭相关程序（含托盘图标）后重试")

    _remove_junction_dirs(src)
    try:
        os.rmdir(src)
    except OSError:
        pass

    create_junction(src, dest)
    if not junction_works(src, dest):
        log("联接校验失败（子目录不可访问），正在还原…")
        _rollback_to_source(src, dest)
        raise MigrateError(
            "该位置不支持跨卷联接（可能被安全软件的文件防护限制），已还原，数据未丢失")
    log(f"已建立联接：{src} → {dest}")
    return src, dest


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
    rc = _robocopy(target, src, move=True)
    remaining = count_files(target)
    if rc >= 8 or remaining:
        raise MigrateError(
            "数据已搬回大部分，但仍有文件被占用未能搬回，"
            f"请检查原位置 {src} 和目标 {target}")

    remove_tree(target)
    log(f"已还原：{src}")
    return src


def candidate_folders():
    """常见的、可以安全迁移的大目录（只返回存在的）。"""
    local = os.environ.get("LOCALAPPDATA", "")
    roaming = os.environ.get("APPDATA", "")
    profile = os.environ.get("USERPROFILE", "")
    raw = [
        os.path.join(roaming, "Tencent"),
        os.path.join(local, "Quark"),
        os.path.join(profile, ".vscode", "extensions"),
        os.path.join(profile, ".cache"),
        os.path.join(profile, ".cargo"),
        os.path.join(profile, ".rustup"),
        os.path.join(profile, ".gemini"),
        os.path.join(profile, ".workbuddy-ai"),
        os.path.join(profile, ".codex"),
        os.path.join(local, "Programs"),
        os.path.join(local, "Packages"),
        os.path.join(roaming, "kingsoft"),
        os.path.join(profile, "Documents"),
        os.path.join(profile, "Downloads"),
        os.path.join(profile, "Desktop"),
    ]
    seen, out = set(), []
    for path in raw:
        if not path:
            continue
        norm = os.path.normcase(os.path.abspath(path))
        if norm in seen or not os.path.isdir(path):
            continue
        seen.add(norm)
        out.append(os.path.abspath(path))
    return out
