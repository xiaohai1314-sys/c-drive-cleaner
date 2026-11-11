"""读取清理日志，汇总清理历史。"""
import os
import re


def _fmt_time(stem):
    # 形如 20261111_133500
    if len(stem) >= 15 and stem[8] == "_":
        return (f"{stem[0:4]}-{stem[4:6]}-{stem[6:8]} "
                f"{stem[9:11]}:{stem[11:13]}:{stem[13:15]}")
    return stem


def load_history(log_dir):
    rows = []
    if not os.path.isdir(log_dir):
        return rows
    try:
        names = sorted(os.listdir(log_dir), reverse=True)
    except OSError:
        return rows
    for name in names:
        if not name.endswith(".log"):
            continue
        path = os.path.join(log_dir, name)
        freed = "-"
        items = 0
        try:
            with open(path, encoding="utf-8", errors="ignore") as handle:
                for line in handle:
                    if "清理结束，共释放" in line:
                        match = re.search(r"共释放\s*([\d.]+\s*[A-Z]?B)", line)
                        if match:
                            freed = match.group(1)
                    elif "清理：" in line:
                        items += 1
        except OSError:
            continue
        rows.append({
            "time": _fmt_time(name[:-4]),
            "freed": freed,
            "items": items,
            "path": path,
        })
    return rows
