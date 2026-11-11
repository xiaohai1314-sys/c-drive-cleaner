"""界面素材：圆角按钮、勾选框、文件图标、导航图标。用 Pillow 画，避免商标图案。"""
import io
import os

FAMILY = "Microsoft YaHei UI"
_CACHE = {}
_FONT_FILE = ""
_FONT_BOLD = ""


def bind(root):
    """选出当前系统能显示中文的界面字体。"""
    global FAMILY, _FONT_FILE, _FONT_BOLD
    try:
        import tkinter.font as tkfont
        families = set(tkfont.families(root))
    except Exception:
        families = set()
    for name in ("Microsoft YaHei UI", "Microsoft YaHei", "Noto Sans CJK SC",
                 "Noto Sans CJK JP", "WenQuanYi Micro Hei", "Droid Sans Fallback"):
        if name in families:
            FAMILY = name
            break
    regular = (
        r"C:\Windows\Fonts\msyh.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    )
    bold = (
        r"C:\Windows\Fonts\msyhbd.ttc",
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Bold.ttc",
        "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc",
    )
    _FONT_FILE = next((path for path in regular if os.path.isfile(path)), "")
    _FONT_BOLD = next((path for path in bold if os.path.isfile(path)), _FONT_FILE)


def tk_font(size, bold=False):
    if bold:
        return (FAMILY, size, "bold")
    return (FAMILY, size)


def _pil_font(size, bold=False):
    from PIL import ImageFont
    path = _FONT_BOLD if bold and _FONT_BOLD else _FONT_FILE
    if path:
        try:
            return ImageFont.truetype(path, size)
        except Exception:
            pass
    return ImageFont.load_default()


def _photo(image):
    from PIL import ImageTk
    return ImageTk.PhotoImage(image)


def _cached(key, factory):
    photo = _CACHE.get(key)
    if photo is None:
        photo = factory()
        _CACHE[key] = photo
    return photo


def checkbox(checked, size=18):
    def draw():
        from PIL import Image, ImageDraw
        image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        pen = ImageDraw.Draw(image)
        box = (1, 1, size - 2, size - 2)
        if checked:
            pen.rounded_rectangle(box, radius=4, fill=(35, 214, 160, 255))
            pen.line((size * 0.22, size * 0.52, size * 0.42, size * 0.72), fill="white", width=2)
            pen.line((size * 0.42, size * 0.72, size * 0.78, size * 0.28), fill="white", width=2)
        else:
            pen.rounded_rectangle(box, radius=4, outline=(198, 208, 218, 255), width=2, fill=(255, 255, 255, 255))
        return _photo(image)
    return _cached(("check", checked, size), draw)


def button(text, fill, width=156, height=48):
    def draw():
        from PIL import Image, ImageDraw
        image = Image.new("RGBA", (width, height), (0, 0, 0, 0))
        pen = ImageDraw.Draw(image)
        pen.rounded_rectangle((0, 0, width - 1, height - 1), radius=height // 2, fill=fill)
        font = _pil_font(16, True)
        pen.text((width / 2, height / 2), text, fill="white", font=font, anchor="mm")
        return _photo(image)
    return _cached(("btn", text, fill, width, height), draw)


def file_icon(kind, ext=""):
    colors = {
        "video": (142, 108, 214),
        "music": (46, 184, 168),
        "document": (72, 136, 232),
        "image": (240, 148, 64),
        "archive": (214, 96, 72),
        "installer": (214, 84, 96),
        "other": (120, 144, 168),
    }
    color = colors.get(kind, colors["other"])
    label = (ext or kind[:3]).upper().replace(".", "")[:4] or "FILE"

    def draw():
        from PIL import Image, ImageDraw
        image = Image.new("RGBA", (28, 32), (0, 0, 0, 0))
        pen = ImageDraw.Draw(image)
        pen.rounded_rectangle((2, 2, 22, 30), radius=3, fill=(255, 255, 255, 255), outline=color, width=2)
        pen.polygon([(16, 2), (22, 8), (16, 8)], fill=color)
        pen.rectangle((4, 16, 20, 28), fill=color)
        font = _pil_font(7, True)
        pen.text((12, 22), label[:3], fill="white", font=font, anchor="mm")
        return _photo(image)
    return _cached(("file", kind, label), draw)


def nav_icon(key):
    def draw():
        from PIL import Image, ImageDraw
        image = Image.new("RGBA", (28, 28), (0, 0, 0, 0))
        pen = ImageDraw.Draw(image)
        blue = (32, 150, 230, 255)
        if key == "clean":
            pen.ellipse((4, 4, 18, 18), outline=blue, width=2)
            pen.line((16, 16, 23, 23), fill=blue, width=2)
        elif key == "frag":
            for index, height in enumerate((8, 14, 10, 18, 12)):
                pen.rectangle((3 + index * 5, 24 - height, 6 + index * 5, 24), fill=blue)
        elif key == "privacy":
            pen.ellipse((6, 3, 22, 16), outline=blue, width=2)
            pen.polygon([(8, 14), (14, 24), (20, 14)], outline=blue)
        elif key == "big":
            pen.rounded_rectangle((4, 6, 24, 22), radius=2, outline=blue, width=2)
            pen.line((4, 12, 24, 12), fill=blue, width=2)
        elif key == "dup":
            pen.rounded_rectangle((3, 6, 16, 19), radius=2, outline=blue, width=2)
            pen.rounded_rectangle((11, 11, 24, 24), radius=2, outline=blue, width=2)
        elif key == "wechat":
            pen.ellipse((3, 5, 24, 22), outline=blue, width=2)
            pen.polygon([(8, 18), (6, 24), (14, 19)], fill=blue)
        elif key == "qq":
            pen.ellipse((4, 4, 24, 24), outline=blue, width=2)
            font = _pil_font(12, True)
            pen.text((14, 14), "Q", fill=blue, font=font, anchor="mm")
        else:
            pen.line((6, 8, 22, 8), fill=blue, width=2)
            pen.line((8, 14, 20, 14), fill=blue, width=2)
            pen.line((10, 20, 18, 20), fill=blue, width=2)
        return _photo(image)
    return _cached(("nav", key), draw)


def category_icon(name, checked=False):
    """清理项用的彩色圆角图标，右下角叠加勾选。不使用任何品牌标识。"""
    palette = (
        (66, 145, 240), (32, 186, 154), (126, 104, 214), (240, 146, 54),
        (64, 176, 96), (230, 96, 96), (64, 168, 214), (176, 112, 214),
    )
    color = palette[sum(ord(ch) for ch in name) % len(palette)]

    def draw():
        from PIL import Image, ImageDraw
        size = 64
        image = Image.new("RGBA", (size, size), (0, 0, 0, 0))
        pen = ImageDraw.Draw(image)
        pen.rounded_rectangle((2, 2, size - 3, size - 3), radius=16, fill=color)
        font = _pil_font(20, True)
        pen.text((size / 2, size / 2 - 2), (name[:1] or "项"), fill="white", font=font, anchor="mm")
        if checked:
            pen.ellipse((size - 22, size - 22, size - 4, size - 4), fill=(35, 214, 160, 255))
            pen.line((size - 17, size - 13, size - 13, size - 9), fill="white", width=2)
            pen.line((size - 13, size - 9, size - 7, size - 16), fill="white", width=2)
        return _photo(image)
    return _cached(("cat", name, checked), draw)


def thumb_for(path, size=88):
    """普通图片，或能还原的微信 .dat。失败返回 None。"""
    import datdecode
    from PIL import Image, ImageDraw
    ext = os.path.splitext(path)[1].lower()
    try:
        if ext == ".dat":
            blob = datdecode.decode_file(path)
            if not blob:
                return None
            image = Image.open(io.BytesIO(blob))
        else:
            image = Image.open(path)
        image = image.convert("RGB")
    except Exception:
        return None
    image.thumbnail((size, size))
    canvas = Image.new("RGB", (size, size), (236, 242, 248))
    canvas.paste(image, ((size - image.width) // 2, (size - image.height) // 2))
    pen = ImageDraw.Draw(canvas)
    try:
        weight = os.path.getsize(path)
    except OSError:
        weight = 0
    label = _compact(weight)
    pen.rectangle((0, size - 16, size, size), fill=(0, 0, 0))
    pen.text((4, size - 14), label, fill="white", font=_pil_font(10, True))
    return _photo(canvas)


def _compact(num):
    num = int(num or 0)
    if num < 1024:
        return f"{num}B"
    value = float(num)
    for unit in ("K", "M", "G"):
        value /= 1024.0
        if value < 1024:
            return f"{value:.1f}{unit}" if value < 10 else f"{int(value)}{unit}"
    return f"{num}B"


class Tick(object):
    """真勾选框。variable 是 tk.BooleanVar。"""

    def __init__(self, master, variable, command=None, bg="#ffffff", size=18):
        import tkinter as tk
        self.var = variable
        self.command = command
        self.size = size
        self.bg = bg
        image = checkbox(bool(variable.get()), size)
        self.widget = tk.Label(master, image=image, bg=bg, bd=0, cursor="hand2")
        self.widget._img = image
        self.widget.bind("<Button-1>", self._click)

    def _click(self, _event=None):
        self.var.set(not bool(self.var.get()))
        image = checkbox(bool(self.var.get()), self.size)
        self.widget._img = image
        self.widget.configure(image=image)
        if self.command:
            self.command()

    def pack(self, **kwargs):
        self.widget.pack(**kwargs)

    def grid(self, **kwargs):
        self.widget.grid(**kwargs)

    def place_over(self, holder):
        self.widget.place(in_=holder, relx=1, rely=1, x=-2, y=-2, anchor="se")


class FileTable(object):
    """自绘文件表：勾选图、图标、斑马纹、悬停。"""

    def __init__(self, master, headers, on_toggle=None, on_sort=None, on_pick=None, bg="#f7fbfe"):
        import tkinter as tk
        from tkinter import ttk
        self.on_toggle = on_toggle
        self.on_sort = on_sort
        self.on_pick = on_pick
        self.bg = bg
        self.headers = headers
        self.frame = tk.Frame(master, bg=bg)
        self.frame.pack(fill="both", expand=True, padx=16, pady=(4, 8))
        head = tk.Frame(self.frame, bg="#f5f8fb")
        head.pack(fill="x")
        for key, title, width in headers:
            label = tk.Label(head, text=title, bg="#f5f8fb", fg="#5c6b7a", anchor="w",
                             font=tk_font(9), width=max(4, width // 8))
            label.pack(side="left", padx=4, pady=6)
            if on_sort and key in ("size", "mtime", "name"):
                label.configure(cursor="hand2")
                label.bind("<Button-1>", lambda _e, name=key: on_sort(name))
        canvas = tk.Canvas(self.frame, bg=bg, highlightthickness=0)
        scroll = ttk.Scrollbar(self.frame, orient="vertical", command=canvas.yview)
        self.inner = tk.Frame(canvas, bg=bg)
        self.inner.bind("<Configure>", lambda _e: canvas.configure(scrollregion=canvas.bbox("all")))
        window = canvas.create_window((0, 0), window=self.inner, anchor="nw")
        canvas.bind("<Configure>", lambda event: canvas.itemconfigure(window, width=event.width))
        canvas.configure(yscrollcommand=scroll.set)
        canvas.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")
        self._rows = []

    def set_rows(self, rows):
        import tkinter as tk
        for child in self.inner.winfo_children():
            child.destroy()
        self._rows = list(rows or [])
        for index, row in enumerate(self._rows):
            bg = row.get("bg") or ("#f3f8fd" if index % 2 else "#ffffff")
            line = tk.Frame(self.inner, bg=bg, height=40)
            line.pack(fill="x", pady=1)
            line.pack_propagate(False)
            var = tk.BooleanVar(value=bool(row.get("checked")))

            def fire(key=row.get("id"), variable=var):
                if self.on_toggle:
                    self.on_toggle(key, bool(variable.get()))

            tick = Tick(line, var, command=fire, bg=bg, size=16)
            tick.pack(side="left", padx=(8, 4))
            icon = row.get("icon")
            if icon is not None:
                tk.Label(line, image=icon, bg=bg).pack(side="left", padx=(2, 6))
            name = tk.Label(line, text=row.get("name") or "", bg=bg, fg="#243140", anchor="w",
                            font=tk_font(9))
            name.pack(side="left", fill="x", expand=True)
            for text, width in reversed(row.get("cells") or []):
                tk.Label(line, text=text, bg=bg, fg="#5c6b7a", anchor="e",
                         font=tk_font(9), width=max(6, width // 8)).pack(side="right", padx=6)
            hover = "#e7f2fb"

            def enter(_event, widget=line, base=bg, hot=hover):
                _paint(widget, hot)

            def leave(_event, widget=line, base=bg):
                _paint(widget, base)

            line.bind("<Enter>", enter)
            line.bind("<Leave>", leave)
            if self.on_pick:
                line.bind("<Button-1>", lambda _e, key=row.get("id"): self.on_pick(key))


def _paint(widget, color):
    try:
        widget.configure(bg=color)
    except Exception:
        return
    for child in widget.winfo_children():
        try:
            child.configure(bg=color)
        except Exception:
            pass
