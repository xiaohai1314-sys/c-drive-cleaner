"""微信图片 .dat 的单字节异或还原。

只根据 JPEG / PNG / GIF 文件头猜测密钥，解不开就返回空，不当成图片显示。
"""

_MAGICS = (
    b"\xff\xd8\xff",
    b"\x89PNG",
    b"GIF8",
)


def xor_key(data: bytes):
    """返回 0-255 的密钥；不像图片头时返回 None。"""
    if not data or len(data) < 4:
        return None
    for magic in _MAGICS:
        key = data[0] ^ magic[0]
        if all((data[index] ^ key) == magic[index] for index in range(len(magic))):
            return key
    return None


def decode_dat(data: bytes):
    """整段按同一个字节异或。识别不出图片头时返回 None，原数据不会被改写。"""
    if not isinstance(data, (bytes, bytearray)):
        return None
    key = xor_key(data)
    if key is None:
        return None
    return bytes(byte ^ key for byte in data)


def decode_file(path: str, limit=12 * 1024 * 1024):
    """读取本地 .dat。超过 limit 的文件不整段读入内存。"""
    try:
        with open(path, "rb") as handle:
            blob = handle.read(limit + 1)
    except OSError:
        return None
    if len(blob) > limit:
        return None
    return decode_dat(blob)
