"""规则云端更新：从配置的地址拉取 rules.json 并合并到本地。"""
import json
import os
import urllib.request

BASE = os.path.dirname(os.path.abspath(__file__))
RULES_PATH = os.path.join(BASE, "rules.json")
CONFIG_PATH = os.path.join(BASE, "rules_update.json")


def load_config():
    try:
        with open(CONFIG_PATH, encoding="utf-8") as handle:
            return json.load(handle)
    except Exception:
        return {}


def update_rules(url=None):
    """返回 (新增/更新条数, 总条数, 错误信息或空串)。"""
    config = load_config()
    url = url or config.get("url") or ""
    if not url:
        return 0, 0, "未配置规则更新地址（在 rules_update.json 里填写 url）"
    try:
        with urllib.request.urlopen(url, timeout=15) as response:
            remote = json.loads(response.read().decode("utf-8"))
    except Exception as exc:
        return 0, 0, f"下载失败：{exc}"
    if not isinstance(remote, dict) or not isinstance(remote.get("categories"), list):
        return 0, 0, "远程规则格式不正确"

    try:
        with open(RULES_PATH, encoding="utf-8") as handle:
            local = json.load(handle)
    except Exception:
        local = {"version": 1, "categories": []}

    existing = {item.get("key") for item in local.get("categories", [])}
    added = 0
    for item in remote["categories"]:
        key = item.get("key")
        if not key:
            continue
        if key not in existing:
            local["categories"].append(item)
            existing.add(key)
            added += 1

    try:
        with open(RULES_PATH, "w", encoding="utf-8") as handle:
            json.dump(local, handle, ensure_ascii=False, indent=2)
    except Exception as exc:
        return 0, 0, f"写入失败：{exc}"
    return added, len(local["categories"]), ""
