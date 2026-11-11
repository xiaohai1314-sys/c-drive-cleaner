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


def thumb_for(path, size=90, weight=None):
    """普通图片，或能还原的微信 .dat。铺满裁切成正方形，左下角半透明标出大小。失败返回 None。"""
    import datdecode
    from PIL import Image
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
    side = min(image.width, image.height)
    left = (image.width - side) // 2
    top = (image.height - side) // 2
    image = image.crop((left, top, left + side, top + side)).resize((size, size))
    if weight is None:
        try:
            weight = os.path.getsize(path)
        except OSError:
            weight = 0
    return _photo(_size_badge(image, weight))


def video_tile(weight, size=90):
    """视频没有可用缩略图时的占位块：深色底、播放三角、左下角大小。"""
    from PIL import Image, ImageDraw
    image = Image.new("RGB", (size, size), (52, 64, 80))
    pen = ImageDraw.Draw(image)
    cx, cy = size // 2, size // 2 - 6
    pen.polygon([(cx - 9, cy - 12), (cx - 9, cy + 12), (cx + 13, cy)], fill=(235, 240, 245))
    return _photo(_size_badge(image, weight))


def _size_badge(image, weight):
    from PIL import Image, ImageDraw
    base = image.convert("RGBA")
    layer = Image.new("RGBA", base.size, (0, 0, 0, 0))
    pen = ImageDraw.Draw(layer)
    label = _compact_size(weight)
    font = _pil_font(10)
    pen.rectangle((0, base.height - 15, base.width, base.height), fill=(0, 0, 0, 110))
    pen.text((4, base.height - 14), label, fill=(255, 255, 255, 255), font=font)
    return Image.alpha_composite(base, layer).convert("RGB")


def _compact_size(num):
    num = int(num or 0)
    if num < 1024:
        return f"{num}B"
    value = float(num)
    for unit in ("KB", "MB", "GB"):
        value /= 1024.0
        if value < 1024:
            return f"{value:.1f}{unit}"
    return f"{value:.1f}TB"


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
    """自绘文件表：表头和每一行共用同一套 grid 列宽，保证列对齐。

    headers: (key, title, width)。key 为 "check" 的列放勾选框，"name" 列放图标和
    文件名并自动拉伸，其余列固定宽度、左对齐。
    """

    ROW_HEIGHT = 34
    HEAD_BG = "#f5f8fb"

    def __init__(self, master, headers, on_toggle=None, on_sort=None, on_pick=None,
                 bg="#f7fbfe", on_header=None, sortable=("size", "mtime", "name")):
        import tkinter as tk
        from tkinter import ttk
        self.on_toggle = on_toggle
        self.on_sort = on_sort
        self.on_pick = on_pick
        self.on_header = on_header
        self.sortable = tuple(sortable)
        self.bg = bg
        self.headers = list(headers)
        self._titles = {key: title for key, title, _w in self.headers}
        self._sort = (None, True)
        self.frame = tk.Frame(master, bg=bg)
        self.frame.pack(fill="both", expand=True, padx=16, pady=(4, 8))
        self.head = tk.Frame(self.frame, bg=self.HEAD_BG, height=32)
        self.head.pack(fill="x")
        self.head.pack_propagate(False)
        self.head_inner = tk.Frame(self.head, bg=self.HEAD_BG)
        self.head_inner.pack(fill="both", expand=True)
        self._configure_columns(self.head_inner)
        self.head_labels = {}
        self.head_slot = None
        for column, (key, title, _width) in enumerate(self.headers):
            if key == "check":
                slot = tk.Frame(self.head_inner, bg=self.HEAD_BG)
                slot.grid(row=0, column=column, sticky="w", padx=(4, 0))
                self.head_slot = slot
                continue
            label = tk.Label(self.head_inner, text=title, bg=self.HEAD_BG, fg="#5c6b7a",
                             anchor="w", font=tk_font(9))
            label.grid(row=0, column=column, sticky="we", padx=(40 if key == "name" else 6, 6), pady=6)
            self.head_labels[key] = label
            if (on_sort and key in self.sortable) or (on_header and key not in self.sortable):
                label.configure(cursor="hand2")
                label.bind("<Button-1>", lambda event, name=key: self._head_click(name, event))
        body = tk.Frame(self.frame, bg=bg)
        body.pack(fill="both", expand=True)
        canvas = tk.Canvas(body, bg=bg, highlightthickness=0)
        scroll = ttk.Scrollbar(body, orient="vertical", command=canvas.yview)
        self.inner = tk.Frame(canvas, bg=bg)
        self.inner.bind("<Configure>", lambda _e: canvas.configure(scrollregion=canvas.bbox("all")))
        window = canvas.create_window((0, 0), window=self.inner, anchor="nw")
        canvas.bind("<Configure>", lambda event: self._resize(canvas, window, event.width))
        canvas.configure(yscrollcommand=scroll.set)
        canvas.pack(side="left", fill="both", expand=True)
        scroll.pack(side="right", fill="y")
        # 表头右侧留出和滚动条一样宽的空位，列才能和下面的行对齐。
        self._scroll = scroll
        self._canvas = canvas
        self._rows = []
        self._names = []
        self._name_width = 300

    def _configure_columns(self, widget):
        for column, (key, _title, width) in enumerate(self.headers):
            if key == "name":
                widget.grid_columnconfigure(column, weight=1, minsize=width // 2)
            else:
                widget.grid_columnconfigure(column, weight=0, minsize=width)

    def _resize(self, canvas, window, width):
        canvas.itemconfigure(window, width=width)
        try:
            pad = max(self._scroll.winfo_width(), self._scroll.winfo_reqwidth())
        except Exception:
            pad = 0
        self.head_inner.pack_configure(padx=(0, pad))
        fixed = sum(w for key, _t, w in self.headers if key != "name")
        name_width = max(120, width - fixed - 60)
        if abs(name_width - self._name_width) > 8:
            self._name_width = name_width
            for label, text in self._names:
                try:
                    label.configure(text=ellipsize_middle(text, name_width))
                except Exception:
                    pass

    def _head_click(self, key, event):
        if key in self.sortable and self.on_sort:
            self.on_sort(key)
        elif self.on_header:
            self.on_header(key, event)

    def set_header(self, key, text):
        self._titles[key] = text
        self._paint_headers()

    def set_sort(self, key, desc=True):
        self._sort = (key, desc)
        self._paint_headers()

    def _paint_headers(self):
        sort_key, desc = self._sort
        for key, label in self.head_labels.items():
            text = self._titles.get(key, "")
            if key == sort_key:
                text += " ▾" if desc else " ▴"
            label.configure(text=text, fg="#1592e6" if key == sort_key else "#5c6b7a")

    def set_rows(self, rows):
        import tkinter as tk
        for child in self.inner.winfo_children():
            child.destroy()
        self._rows = list(rows or [])
        self._names = []
        for index, row in enumerate(self._rows):
            bg = row.get("bg") or "#ffffff"
            line = tk.Frame(self.inner, bg=bg, height=self.ROW_HEIGHT)
            line.pack(fill="x")
            line.grid_propagate(False)
            line.grid_rowconfigure(0, weight=1)
            self._configure_columns(line)
            var = tk.BooleanVar(value=bool(row.get("checked")))

            def fire(key=row.get("id"), variable=var):
                if self.on_toggle:
                    self.on_toggle(key, bool(variable.get()))

            cells = list(row.get("cells") or [])
            cell_index = 0
            for column, (key, _title, _width) in enumerate(self.headers):
                if key == "check":
                    tick = Tick(line, var, command=fire, bg=bg, size=16)
                    tick.widget.grid(row=0, column=column, sticky="w", padx=(10, 0))
                elif key == "name":
                    holder = tk.Frame(line, bg=bg)
                    holder.grid(row=0, column=column, sticky="we")
                    icon = row.get("icon")
                    if icon is not None:
                        tk.Label(holder, image=icon, bg=bg).pack(side="left", padx=(6, 6))
                    full = row.get("name") or ""
                    name = tk.Label(holder, text=ellipsize_middle(full, self._name_width), bg=bg,
                                    fg="#243140", anchor="w", font=tk_font(9))
                    name.pack(side="left", fill="x", expand=True)
                    self._names.append((name, full))
                else:
                    text = cells[cell_index][0] if cell_index < len(cells) else ""
                    cell_index += 1
                    tk.Label(line, text=text, bg=bg, fg="#5c6b7a", anchor="w",
                             font=tk_font(9)).grid(row=0, column=column, sticky="we", padx=6)
            if row.get("group_end") and index < len(self._rows) - 1:
                tk.Frame(self.inner, bg="#dfe8f1", height=1).pack(fill="x")
            hover = "#e7f2fb"

            def enter(_event, widget=line, hot=hover):
                _paint(widget, hot)

            def leave(_event, widget=line, base=bg):
                _paint(widget, base)

            line.bind("<Enter>", enter)
            line.bind("<Leave>", leave)
            if self.on_pick:
                line.bind("<Button-1>", lambda _e, key=row.get("id"): self.on_pick(key))


def ellipsize_middle(text, pixels, size=9):
    """按像素宽度把长路径缩成「C:\\…尾部」，保留盘符和文件名结尾。"""
    text = str(text or "")
    try:
        font = _CACHE.get(("measure", FAMILY, size))
        if font is None:
            import tkinter.font as tkfont
            font = tkfont.Font(family=FAMILY, size=size)
            _CACHE[("measure", FAMILY, size)] = font
        measure = font.measure
    except Exception:
        def measure(value):
            return sum(13 if ord(ch) > 0x2E80 else 7 for ch in value)
    if measure(text) <= pixels:
        return text
    head = text[:3] if len(text) > 3 and text[1:3] == ":\\" else ""
    tail = text[len(head):]
    while tail and measure(head + "…" + tail) > pixels:
        tail = tail[1:]
    return head + "…" + tail


def _paint(widget, color):
    try:
        widget.configure(bg=color)
    except Exception:
        return
    for child in widget.winfo_children():
        _paint(child, color)
