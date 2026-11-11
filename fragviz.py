"""把 defrag 分析文本变成可绘制的分布，而不是固定花纹。"""
import re


def parse_defrag_report(text: str) -> dict:
    """从 defrag /A 的中文或英文输出里取出碎片率、碎片量和碎片数。

    解析不到的字段保持 None，调用方不要拿假数字去画格子。
    """
    raw = text or ""
    folded = raw.replace("\u3000", " ")
    percent = _search_percent(folded)
    space = _search_space(folded)
    count = _search_count(folded)
    free = _search_free(folded)
    return {"percent": percent, "space_gb": space, "count": count, "free_gb": free}


def _search_percent(text: str) -> float | None:
    patterns = (
        r"(?:碎片率|碎片总计|文件碎片百分比|Percent file fragmentation)"
        r"[^\d%]{0,40}(\d+(?:\.\d+)?)\s*%",
        r"(\d+(?:\.\d+)?)\s*%\s*(?:fragmented|碎片)",
    )
    for pattern in patterns:
        match = re.search(pattern, text, re.IGNORECASE)
        if match:
            return float(match.group(1))
    # 旧输出里第一个百分号通常就是碎片率；可用空间百分比字样出现时跳过它。
    for match in re.finditer(r"([^\n]{0,24})=\s*(\d+(?:\.\d+)?)\s*%", text):
        label = match.group(1)
        if "可用" in label or "free" in label.lower():
            continue
        return float(match.group(2))
    return None


def _search_space(text: str) -> float | None:
    patterns = (
        r"(?:碎片空间|文件碎片|Total fragmented space|碎片文件)"
        r"[^\d]{0,20}([\d.]+)\s*GB",
    )
    for pattern in patterns:
        match = re.search(pattern, text, re.IGNORECASE)
        if match:
            return float(match.group(1))
    return None


def _search_free(text: str) -> float | None:
    match = re.search(r"(?:可用空间|Free space)[^\d]{0,20}([\d.]+)\s*GB", text, re.IGNORECASE)
    if match:
        return float(match.group(1))
    return None


def _search_count(text: str) -> int | None:
    patterns = (
        r"(?:碎片文件数|总碎片数|碎片数|Fragmented files|Total fragments)"
        r"\s*[=:：]?\s*([\d,]+)",
    )
    for pattern in patterns:
        match = re.search(pattern, text, re.IGNORECASE)
        if match:
            return int(match.group(1).replace(",", ""))
    return None


def cells_from_analysis(percent, rows: int, cols: int):
    """按碎片率铺格子。percent 为 None 时返回 None，表示没有分析数据。

    高亮格子的数量等于 round(总数 * 碎片率)，位置按步长铺开，
    这样分布会随真实百分比变化，而不是一幅写死的花纹。
    """
    if percent is None:
        return None
    rows = int(rows)
    cols = int(cols)
    if rows <= 0 or cols <= 0:
        return []
    try:
        rate = float(percent)
    except (TypeError, ValueError):
        return None
    rate = max(0.0, min(100.0, rate))
    total = rows * cols
    hot_n = int(round(total * rate / 100.0))
    hot = set()
    if hot_n >= total:
        hot = set(range(total))
    elif hot_n > 0:
        step = total / hot_n
        for index in range(hot_n):
            hot.add(min(total - 1, int(index * step)))
        cursor = 0
        while len(hot) < hot_n and cursor < total:
            hot.add(cursor)
            cursor += 1
    return ["hot" if index in hot else "ok" for index in range(total)]
