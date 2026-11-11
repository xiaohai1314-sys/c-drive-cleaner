"""隐私痕迹：按浏览器和注册表统计，清理时跳过密码库。

购物记录、下载记录能从浏览器历史库里数出来时就给出条数。
删下载记录或购物记录时只改对应的表，不把整份历史一起删掉，
除非用户同时勾了该浏览器的浏览历史。
"""
import os
import shutil
import sqlite3
import tempfile

import safepath

TRACE_FILENAMES = (
    "History",
    "History-journal",
    "Cookies",
    "Cookies-journal",
    "Top Sites",
    "Visited Links",
    "Favicons",
)

SHOP_DOMAINS = (
    "taobao.com", "tmall.com", "jd.com", "yangkeduo.com", "pinduoduo.com",
    "amazon.com", "amazon.cn", "smzdm.com", "suning.com", "dangdang.com",
)
FINANCE_DOMAINS = (
    "alipay.com", "paypal.com", "icbc.com.cn", "ccb.com", "cmbchina.com",
    "unionpay.com", "abchina.com",
)
MEDICAL_DOMAINS = (
    "haodf.com", "dxy.cn", "xywy.com", "chunyuyisheng.com", "guahao.com",
)
SENSITIVE_NAME_WORDS = ("身份证", "密码", "银行卡", "合同", "护照", "工资")

SECTIONS = (
    ("high", "高危隐私", "容易泄露的个人敏感信息"),
    ("history", "上网浏览历史", "使用浏览器时产生的痕迹"),
    ("download", "下载历史记录", "浏览器下载记录，不删除已经下载的文件"),
    ("shop", "网络购物记录", "浏览记录里能识别的购物网站"),
    ("computer", "电脑使用痕迹", "使用电脑系统产生的痕迹"),
    ("software", "软件使用痕迹", "程序运行记录"),
    ("media", "影音播放痕迹", "播放器最近播放"),
)


def collect_chromium_files(profile: str) -> list:
    """一个 Chromium 配置目录里可以清理的痕迹文件，不含密码库。"""
    found = []
    for name in TRACE_FILENAMES:
        path = os.path.join(profile, name)
        if os.path.isfile(path) and not safepath.is_password_store(path):
            found.append(path)
    network = os.path.join(profile, "Network")
    for name in ("Cookies", "Cookies-journal"):
        path = os.path.join(network, name)
        if os.path.isfile(path) and not safepath.is_password_store(path):
            found.append(path)
    return found


def _profiles(root: str):
    if not root or not os.path.isdir(root):
        return []
    found = []
    try:
        names = os.listdir(root)
    except OSError:
        return []
    for name in names:
        if name != "Default" and not name.startswith("Profile") and name != "Guest Profile":
            continue
        path = os.path.join(root, name)
        if os.path.isdir(path):
            found.append(path)
    return found


def _firefox_profiles(root: str):
    if not root or not os.path.isdir(root):
        return []
    found = []
    try:
        names = os.listdir(root)
    except OSError:
        return []
    for name in names:
        path = os.path.join(root, name)
        if os.path.isdir(path) and os.path.isfile(os.path.join(path, "places.sqlite")):
            found.append(path)
    return found


def _browser_specs():
    local = os.environ.get("LOCALAPPDATA", "")
    roaming = os.environ.get("APPDATA", "")
    return [
        {"id": "ie", "name": "IE", "kind": "ie"},
        {"id": "chrome", "name": "Chrome", "kind": "chromium",
         "root": os.path.join(local, "Google", "Chrome", "User Data")},
        {"id": "edge", "name": "Microsoft Edge", "kind": "chromium",
         "root": os.path.join(local, "Microsoft", "Edge", "User Data")},
        {"id": "qqbrowser", "name": "QQ浏览器", "kind": "chromium",
         "root": os.path.join(local, "Tencent", "QQBrowser", "User Data")},
        {"id": "quark", "name": "夸克", "kind": "chromium",
         "root": os.path.join(local, "Quark", "User Data")},
        {"id": "firefox", "name": "Firefox", "kind": "firefox",
         "root": os.path.join(roaming, "Mozilla", "Firefox", "Profiles")},
    ]


def _connect_copy(path):
    if not path or not os.path.isfile(path) or safepath.is_password_store(path):
        return None, None
    handle = tempfile.NamedTemporaryFile(delete=False, suffix=".db")
    handle.close()
    try:
        shutil.copyfile(path, handle.name)
        return sqlite3.connect(handle.name), handle.name
    except (OSError, sqlite3.Error):
        try:
            os.remove(handle.name)
        except OSError:
            pass
        return None, None


def _close_copy(conn, tmp):
    try:
        if conn is not None:
            conn.close()
    except sqlite3.Error:
        pass
    if tmp:
        try:
            os.remove(tmp)
        except OSError:
            pass


def _count(conn, sql, params=()):
    try:
        row = conn.execute(sql, params).fetchone()
    except sqlite3.Error:
        return None
    if not row or row[0] is None:
        return 0
    return int(row[0])


def _count_domains(conn, table, domains):
    clause = " OR ".join(["url LIKE ?" for _ in domains])
    params = tuple(f"%{domain}%" for domain in domains)
    return _count(conn, f"SELECT COUNT(*) FROM {table} WHERE {clause}", params)


def _shop_where(column="url"):
    clause = " OR ".join([f"{column} LIKE ?" for _ in SHOP_DOMAINS])
    params = tuple(f"%{domain}%" for domain in SHOP_DOMAINS)
    return clause, params


def history_stats(path: str) -> dict:
    """读取浏览器历史库；打不开或缺少相应表时以状态标记，不伪装成 0。"""
    stats = {
        "urls": 0, "downloads": 0, "shop": 0, "money": 0, "medical": 0,
        "urls_available": False, "downloads_available": False,
        "shop_available": False, "money_available": False,
        "medical_available": False,
    }
    conn, tmp = _connect_copy(path)
    if conn is None:
        return stats
    try:
        stats["urls"] = _count(conn, "SELECT COUNT(*) FROM urls")
        if stats["urls"] is None:
            stats["urls"] = _count(conn, "SELECT COUNT(*) FROM moz_places")
        stats["urls_available"] = stats["urls"] is not None
        stats["downloads"] = _count(conn, "SELECT COUNT(*) FROM downloads")
        if stats["downloads"] is None:
            stats["downloads"] = _count(
                conn,
                "SELECT COUNT(*) FROM moz_annos a "
                "JOIN moz_anno_attributes n ON a.anno_attribute_id = n.id "
                "WHERE n.name LIKE 'downloads/%'",
            )
        clause, params = _shop_where("url")
        stats["shop"] = _count(conn, f"SELECT COUNT(*) FROM urls WHERE {clause}", params)
        if stats["shop"] is None:
            clause, params = _shop_where("url")
            stats["shop"] = _count(conn, f"SELECT COUNT(*) FROM moz_places WHERE {clause}", params)
        stats["shop_available"] = stats["shop"] is not None
        stats["money"] = _count_domains(conn, "urls", FINANCE_DOMAINS)
        if stats["money"] is None:
            stats["money"] = _count_domains(conn, "moz_places", FINANCE_DOMAINS)
        stats["money_available"] = stats["money"] is not None
        stats["medical"] = _count_domains(conn, "urls", MEDICAL_DOMAINS)
        if stats["medical"] is None:
            stats["medical"] = _count_domains(conn, "moz_places", MEDICAL_DOMAINS)
        stats["medical_available"] = stats["medical"] is not None
        stats["downloads_available"] = stats["downloads"] is not None
    finally:
        _close_copy(conn, tmp)
    for key in ("urls", "downloads", "shop", "money", "medical"):
        if stats[key] is None:
            stats[key] = 0
    return stats


def cookie_stats(path: str) -> dict:
    conn, tmp = _connect_copy(path)
    if conn is None:
        return {"count": 0, "available": False}
    try:
        count = _count(conn, "SELECT COUNT(*) FROM cookies")
        if count is None:
            count = _count(conn, "SELECT COUNT(*) FROM moz_cookies")
        return {"count": count or 0, "available": count is not None}
    finally:
        _close_copy(conn, tmp)


def cookie_count(path: str) -> int:
    return cookie_stats(path)["count"]


def apply_sql(path: str, statements) -> bool:
    """在副本上执行语句，成功后写回。密码库直接拒绝。"""
    if not path or safepath.is_password_store(path) or not os.path.isfile(path):
        return False
    conn, tmp = _connect_copy(path)
    if conn is None:
        return False
    try:
        applied = False
        for statement in statements or []:
            try:
                conn.execute(statement)
                applied = True
            except sqlite3.Error:
                continue
        if not applied:
            _close_copy(conn, tmp)
            return False
        conn.commit()
    except sqlite3.Error:
        _close_copy(conn, tmp)
        return False
    try:
        conn.close()
        shutil.copyfile(tmp, path)
        return True
    except OSError:
        return False
    finally:
        if tmp and os.path.exists(tmp):
            try:
                os.remove(tmp)
            except OSError:
                pass


def _download_sql(path: str):
    # 同时覆盖 Chromium 和 Firefox，执行时打不开的语句会被跳过。
    return [
        "DELETE FROM downloads_url_chains",
        "DELETE FROM downloads",
        "DELETE FROM moz_annos WHERE anno_attribute_id IN "
        "(SELECT id FROM moz_anno_attributes WHERE name LIKE 'downloads/%')",
    ]


def _shop_sql():
    return _domain_sql(SHOP_DOMAINS)


def _domain_sql(domains):
    """按域名删掉浏览历史里的访问记录。域名来自固定列表，不拼接用户输入。"""
    likes = " OR ".join([f"url LIKE '%{domain}%'" for domain in domains])
    return [
        f"DELETE FROM visits WHERE url IN (SELECT id FROM urls WHERE {likes})",
        f"DELETE FROM urls WHERE {likes}",
        f"DELETE FROM moz_places WHERE {likes}",
    ]


def _scan_chromium(spec):
    files = []
    history_files = []
    urls = downloads = shop = cookies = money = medical = 0
    history_available = downloads_available = shop_available = False
    money_available = medical_available = cookies_available = False
    for profile in _profiles(spec.get("root", "")):
        picked = collect_chromium_files(profile)
        files.extend(picked)
        history = os.path.join(profile, "History")
        if os.path.isfile(history):
            history_files.append(history)
            stats = history_stats(history)
            urls += stats["urls"]
            downloads += stats["downloads"]
            shop += stats["shop"]
            money += stats.get("money") or 0
            medical += stats.get("medical") or 0
            history_available |= stats.get("urls_available", False)
            downloads_available |= stats.get("downloads_available", False)
            shop_available |= stats.get("shop_available", False)
            money_available |= stats.get("money_available", False)
            medical_available |= stats.get("medical_available", False)
        for cookie in (os.path.join(profile, "Cookies"),
                       os.path.join(profile, "Network", "Cookies")):
            if os.path.isfile(cookie):
                cookie_result = cookie_stats(cookie)
                cookies += cookie_result["count"]
                cookies_available |= cookie_result["available"]
    return {
        "id": f"hist_{spec['id']}",
        "count_available": history_available,
        "downloads_available": downloads_available,
        "shop_available": shop_available,
        "money_available": money_available,
        "medical_available": medical_available,
        "cookies_available": cookies_available,
        "section": "history",
        "name": spec["name"],
        "browser": spec["id"],
        "count": urls,
        "files": files,
        "history_files": history_files,
        "downloads": downloads,
        "shop": shop,
        "money": money,
        "medical": medical,
        "cookies": cookies,
        "action": "files",
    }


def _scan_firefox(spec):
    files = []
    history_files = []
    urls = downloads = shop = cookies = money = medical = 0
    history_available = downloads_available = shop_available = False
    money_available = medical_available = cookies_available = False
    for profile in _firefox_profiles(spec.get("root", "")):
        places = os.path.join(profile, "places.sqlite")
        cookies_db = os.path.join(profile, "cookies.sqlite")
        if os.path.isfile(places) and not safepath.is_password_store(places):
            files.append(places)
            history_files.append(places)
            stats = history_stats(places)
            urls += stats["urls"]
            downloads += stats["downloads"]
            shop += stats["shop"]
            money += stats.get("money") or 0
            medical += stats.get("medical") or 0
            history_available |= stats.get("urls_available", False)
            downloads_available |= stats.get("downloads_available", False)
            shop_available |= stats.get("shop_available", False)
            money_available |= stats.get("money_available", False)
            medical_available |= stats.get("medical_available", False)
        if os.path.isfile(cookies_db) and not safepath.is_password_store(cookies_db):
            files.append(cookies_db)
            cookie_result = cookie_stats(cookies_db)
            cookies += cookie_result["count"]
            cookies_available |= cookie_result["available"]
    return {
        "id": "hist_firefox",
        "count_available": history_available,
        "downloads_available": downloads_available,
        "shop_available": shop_available,
        "money_available": money_available,
        "medical_available": medical_available,
        "cookies_available": cookies_available,
        "section": "history",
        "name": spec["name"],
        "browser": "firefox",
        "count": urls,
        "files": files,
        "history_files": history_files,
        "downloads": downloads,
        "shop": shop,
        "money": money,
        "medical": medical,
        "cookies": cookies,
        "action": "files",
    }


def _scan_ie():
    count = 0
    available = False
    try:
        import winreg
        path = r"Software\Microsoft\Internet Explorer\TypedURLs"
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
            available = True
            index = 0
            while True:
                try:
                    winreg.EnumValue(key, index)
                except OSError:
                    break
                count += 1
                index += 1
    except (ImportError, OSError):
        count = 0
    return {
        "id": "hist_ie",
        "section": "history",
        "name": "IE",
        "browser": "ie",
        "count": count,
        "count_available": available,
        "files": [],
        "history_files": [],
        "downloads": 0,
        "shop": 0,
        "cookies": 0,
        "action": "ie",
    }


def _recent_dir():
    roaming = os.environ.get("APPDATA", "")
    return os.path.join(roaming, "Microsoft", "Windows", "Recent")


def _scan_high(cookie_records):
    cookies = sum(int(item.get("cookies") or 0) for item in cookie_records)
    cookie_files = []
    for item in cookie_records:
        for path in item.get("files") or []:
            if os.path.basename(path).lower().startswith("cookie"):
                cookie_files.append(path)
    recent = _recent_dir()
    recent_files = []
    recent_available = os.path.isdir(recent)
    if recent_available:
        try:
            for name in os.listdir(recent):
                path = os.path.join(recent, name)
                if os.path.isfile(path):
                    recent_files.append(path)
        except OSError:
            recent_files = []
            recent_available = False
    sensitive = [path for path in recent_files
                 if any(word in os.path.basename(path) for word in SENSITIVE_NAME_WORDS)]
    money = sum(int(item.get("money") or 0) for item in cookie_records)
    shop = sum(int(item.get("shop") or 0) for item in cookie_records)
    medical = sum(int(item.get("medical") or 0) for item in cookie_records)
    history_records = [item for item in cookie_records
                       if item.get("browser") in {"chrome", "edge", "qqbrowser", "quark", "firefox"}]
    money_available = any(item.get("money_available", False) for item in history_records)
    shop_available = any(item.get("shop_available", False) for item in history_records)
    medical_available = any(item.get("medical_available", False) for item in history_records)
    cookies_available = any(item.get("cookies_available", False) for item in history_records)
    # 成人内容、密码泄露、其他敏感信息没有可靠的检测手段，不出卡片，免得显示假的 0 条。
    return [
        {"id": "risk_file", "section": "high", "name": "文件敏感隐私",
         "count": len(sensitive), "count_available": recent_available,
         "files": sensitive, "action": "files"},
        {"id": "risk_money", "section": "high", "name": "财产安全隐私",
         "count": money, "count_available": money_available,
         "files": [], "action": "files"},
        {"id": "risk_shop", "section": "high", "name": "网络购物痕迹",
         "count": shop, "count_available": shop_available,
         "files": [], "action": "files"},
        {"id": "risk_medical", "section": "high", "name": "医疗信息隐私",
         "count": medical, "count_available": medical_available,
         "files": [], "action": "files"},
        {"id": "cookies", "section": "high", "name": "Cookie",
         "count": cookies, "count_available": cookies_available,
         "files": cookie_files, "action": "files"},
    ]


def _scan_computer():
    import registry
    rows = []
    for key, name in registry.GROUPS:
        count, available = registry.group_count_status(key)
        rows.append({
            "id": key,
            "section": "computer",
            "name": name,
            "count": count,
            "count_available": available,
            "files": [],
            "action": "registry",
            "group": key,
        })
    return rows


def _scan_software():
    count = 0
    available = False
    try:
        import winreg
        base = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist"
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, base) as root:
            available = True
            index = 0
            while True:
                try:
                    guid = winreg.EnumKey(root, index)
                except OSError:
                    break
                index += 1
                try:
                    with winreg.OpenKey(root, guid + r"\Count") as key:
                        value_index = 0
                        while True:
                            try:
                                winreg.EnumValue(key, value_index)
                            except OSError:
                                break
                            count += 1
                            value_index += 1
                except OSError:
                    continue
    except (ImportError, OSError):
        pass
    prefetch = os.path.join(os.environ.get("SystemRoot", r"C:\Windows"), "Prefetch")
    if os.path.isdir(prefetch):
        try:
            names = os.listdir(prefetch)
            available = True
            count += sum(1 for name in names if name.lower().endswith(".pf"))
        except OSError:
            pass
    return {
        "id": "software",
        "section": "software",
        "name": "软件使用痕迹",
        "count": count,
        "count_available": available,
        "files": [],
        "action": "userassist",
    }


def _scan_media():
    count = 0
    files = []
    available = False
    try:
        import winreg
        path = r"Software\Microsoft\MediaPlayer\Player\RecentFileList"
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path) as key:
            available = True
            index = 0
            while True:
                try:
                    winreg.EnumValue(key, index)
                except OSError:
                    break
                count += 1
                index += 1
    except (ImportError, OSError):
        pass
    recent = _recent_dir()
    if os.path.isdir(recent):
        try:
            names = os.listdir(recent)
            available = True
        except OSError:
            names = []
        for name in names:
            stem = os.path.splitext(name)[0]
            ext = os.path.splitext(stem)[1].lower()
            if ext in {".mp4", ".mkv", ".avi", ".mp3", ".flac", ".wav", ".mov"}:
                path = os.path.join(recent, name)
                if os.path.isfile(path):
                    files.append(path)
                    count += 1
    vlc = os.path.join(os.environ.get("APPDATA", ""), "vlc", "vlc-qt-interface.ini")
    if os.path.isfile(vlc):
        try:
            with open(vlc, encoding="utf-8", errors="ignore") as handle:
                count += sum(1 for line in handle if "file:///" in line.lower())
                available = True
        except OSError:
            pass
    return {
        "id": "media",
        "section": "media",
        "name": "影音播放痕迹",
        "count": count,
        "count_available": available,
        "files": files,
        "action": "media",
    }


def scan(progress=None):
    """按步骤扫描。progress(正在扫描的名字, 目前已有的记录列表)。"""
    records = []

    def push(name):
        if progress:
            progress(name, list(records))

    browsers = []
    for spec in _browser_specs():
        push(spec["name"])
        if spec["kind"] == "ie":
            item = _scan_ie()
        elif spec["kind"] == "firefox":
            item = _scan_firefox(spec)
        else:
            item = _scan_chromium(spec)
        browsers.append(item)
        if progress:
            progress(spec["name"], list(browsers))
    push("高危隐私")
    records.extend(_scan_high(browsers))
    records.extend(browsers)
    downloads = sum(int(item.get("downloads") or 0) for item in browsers)
    shop = sum(int(item.get("shop") or 0) for item in browsers)
    downloads_available = any(item.get("downloads_available", False) for item in browsers)
    shop_available = any(item.get("shop_available", False) for item in browsers)
    sql_downloads = []
    sql_shop = []
    for item in browsers:
        for path in item.get("history_files") or []:
            sql_downloads.append({
                "browser": item["browser"], "path": path, "statements": _download_sql(path),
            })
            sql_shop.append({
                "browser": item["browser"], "path": path, "statements": _shop_sql(),
            })
    push("下载历史记录")
    records.append({
        "id": "download", "section": "download", "name": "下载历史记录",
        "count": downloads, "count_available": downloads_available,
        "files": [], "sql_jobs": sql_downloads, "action": "sql",
    })
    push("网络购物记录")
    records.append({
        "id": "shop", "section": "shop", "name": "网络购物记录",
        "count": shop, "count_available": shop_available,
        "files": [], "sql_jobs": sql_shop, "action": "sql",
    })
    domain_jobs = {"risk_shop": SHOP_DOMAINS, "risk_money": FINANCE_DOMAINS,
                   "risk_medical": MEDICAL_DOMAINS}
    for record in records:
        domains = domain_jobs.get(record.get("id"))
        if domains is None:
            continue
        jobs = []
        for item in browsers:
            for path in item.get("history_files") or []:
                jobs.append({"browser": item["browser"], "path": path,
                             "statements": _domain_sql(domains)})
        record["action"] = "sql"
        record["sql_jobs"] = jobs
    push("电脑使用痕迹")
    records.extend(_scan_computer())
    push("软件使用痕迹")
    records.append(_scan_software())
    push("影音播放痕迹")
    records.append(_scan_media())
    push("完成")
    return records


def is_selectable(record) -> bool:
    """没有实际清理动作的统计项不能勾选。"""
    if not isinstance(record, dict):
        return False
    action = record.get("action")
    if action == "sql":
        return bool(record.get("sql_jobs")) and record.get("count_available", True)
    if action == "registry":
        return (record.get("count_available", True)
                and int(record.get("count") or 0) > 0)
    if action in ("userassist", "ie", "media", "wmp"):
        return (record.get("count_available", True)
                and (int(record.get("count") or 0) > 0 or bool(record.get("files"))))
    if action == "files":
        return bool(record.get("files"))
    return False


def clean_plan(records, selected_ids):
    """算出要动哪些文件、哪些 SQL、哪些注册表组。密码库会被丢掉。"""
    by_id = {item["id"]: item for item in records or []}
    selected = [by_id[key] for key in selected_ids or []
                if key in by_id and is_selectable(by_id[key])]
    wiped = {item.get("browser") for item in selected if item.get("section") == "history"}
    files = []
    jobs = []
    groups = []
    extras = []
    for item in selected:
        action = item.get("action")
        if action == "sql":
            for job in item.get("sql_jobs") or []:
                if job.get("browser") in wiped:
                    continue
                jobs.append(job)
        elif action == "registry":
            groups.append(item.get("group"))
        elif action == "userassist":
            extras.append("userassist")
        elif action == "ie":
            extras.append("ie")
        elif action == "media":
            files.extend(item.get("files") or [])
            extras.append("wmp")
        else:
            files.extend(item.get("files") or [])
    unique = []
    seen = set()
    for path in files:
        if not path or safepath.is_password_store(path):
            continue
        key = os.path.normcase(os.path.abspath(path))
        if key in seen:
            continue
        seen.add(key)
        unique.append(path)
    return {"files": unique, "sql_jobs": jobs, "groups": groups, "extras": extras}


def _clear_values(path) -> int:
    try:
        import winreg
    except ImportError:
        return 0
    import registry
    if not registry.backup_key(path):
        return 0
    deleted = 0
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, path, 0, winreg.KEY_ALL_ACCESS) as key:
            names = []
            index = 0
            while True:
                try:
                    name, _value, _typ = winreg.EnumValue(key, index)
                except OSError:
                    break
                names.append(name)
                index += 1
            for name in names:
                try:
                    winreg.DeleteValue(key, name)
                    deleted += 1
                except OSError:
                    pass
    except OSError:
        return deleted
    return deleted


def _clear_userassist() -> int:
    try:
        import winreg
    except ImportError:
        return 0
    import registry
    base = r"Software\Microsoft\Windows\CurrentVersion\Explorer\UserAssist"
    if not registry.backup_key(base, "userassist"):
        return 0
    deleted = 0
    try:
        with winreg.OpenKey(winreg.HKEY_CURRENT_USER, base) as root:
            index = 0
            guids = []
            while True:
                try:
                    guids.append(winreg.EnumKey(root, index))
                except OSError:
                    break
                index += 1
    except OSError:
        return 0
    for guid in guids:
        deleted += _clear_values(base + "\\" + guid + r"\Count")
    return deleted


def apply_plan(plan, remover=None) -> dict:
    """执行清理计划。remover 收到路径列表并返回失败路径。"""
    import recyclebin
    import registry

    files = list(plan.get("files") or [])
    remove = remover or (lambda paths: recyclebin.send_to_recycle(paths))
    failed = []
    try:
        failed_paths = set(remove(files) or []) if files else set()
    except Exception as exc:
        failed_paths = set(files)
        failed.append(f"文件移入回收站失败：{exc}")
    for path in files:
        if path in failed_paths or os.path.lexists(path):
            failed.append(f"文件未能移入回收站：{path}")
    removed = [path for path in files if path not in failed_paths and not os.path.lexists(path)]
    sql_done = 0
    for job in plan.get("sql_jobs") or []:
        path = job.get("path") or "（未知路径）"
        exception_reported = False
        try:
            applied = apply_sql(job.get("path"), job.get("statements") or [])
        except Exception as exc:
            applied = False
            exception_reported = True
            failed.append(f"数据库清理失败：{path}（{exc}）")
        if applied:
            sql_done += 1
        elif not exception_reported:
            failed.append(f"数据库清理失败或数据源不可用：{path}")
    reg_done = 0
    for group in plan.get("groups") or []:
        try:
            cleaned = registry.group_clean(group)
            reg_done += cleaned
            remaining, available = registry.group_count_status(group)
            if not available or remaining:
                failed.append(f"注册表清理未完成：{group}（仍有 {remaining} 条或数据源不可用）")
        except Exception as exc:
            failed.append(f"注册表清理失败：{group}（{exc}）")
    extra = 0
    for key, label, action in (
            ("userassist", "软件使用痕迹", _clear_userassist),
            ("ie", "IE 历史", lambda: _clear_values(r"Software\Microsoft\Internet Explorer\TypedURLs")),
            ("wmp", "媒体播放记录", lambda: _clear_values(r"Software\Microsoft\MediaPlayer\Player\RecentFileList"))):
        if key not in (plan.get("extras") or []):
            continue
        try:
            cleaned = action()
            extra += cleaned
            if not cleaned:
                failed.append(f"{label}清理未完成：没有记录被清除或数据源不可用")
        except Exception as exc:
            failed.append(f"{label}清理失败：{exc}")
    backup = registry.last_backup_info()
    return {"files": len(removed), "sql": sql_done, "registry": reg_done + extra,
            "failed": list(dict.fromkeys(failed)),
            "backup_dir": backup.get("dir", ""),
            "backup_failed": backup.get("failed", [])}
