"""按扩展名把文件分成界面上的类型页。"""
import os

VIDEO_EXT = {".mp4", ".avi", ".mov", ".mkv", ".wmv", ".flv", ".m4v", ".3gp",
             ".mpeg", ".mpg", ".ts", ".webm", ".rmvb", ".vob"}
MUSIC_EXT = {".mp3", ".m4a", ".wav", ".flac", ".aac", ".ogg", ".wma", ".ape", ".mid"}
IMAGE_EXT = {".jpg", ".jpeg", ".png", ".gif", ".bmp", ".webp", ".heic", ".tif",
             ".tiff", ".ico", ".raw", ".psd", ".svg"}
DOC_EXT = {".pdf", ".doc", ".docx", ".xls", ".xlsx", ".ppt", ".pptx", ".txt",
           ".csv", ".rtf", ".ofd", ".md", ".wps", ".et", ".dps", ".dwg", ".dxf"}
ARCHIVE_EXT = {".zip", ".rar", ".7z", ".tar", ".gz", ".bz2", ".xz", ".iso", ".cab"}
INSTALLER_EXT = {".exe", ".msi", ".msix", ".apk", ".dmg"}

KIND_LABEL = {
    "video": "视频",
    "music": "音乐",
    "document": "文档",
    "image": "图片",
    "archive": "压缩包",
    "installer": "安装包",
    "other": "其他",
}


def extension(path: str) -> str:
    return os.path.splitext(str(path))[1].lower()


def kind_of(path: str) -> str:
    ext = extension(path)
    if ext in VIDEO_EXT:
        return "video"
    if ext in MUSIC_EXT:
        return "music"
    if ext in IMAGE_EXT:
        return "image"
    if ext in DOC_EXT:
        return "document"
    if ext in ARCHIVE_EXT:
        return "archive"
    if ext in INSTALLER_EXT:
        return "installer"
    return "other"


def received_subkind(path: str) -> str:
    """接收文件下的子类：办公文档 / 视频 / 图片 / 音乐 / 压缩包 / 其他。"""
    ext = extension(path)
    if ext in DOC_EXT:
        return "doc"
    if ext in VIDEO_EXT:
        return "video"
    if ext in IMAGE_EXT:
        return "image"
    if ext in MUSIC_EXT:
        return "music"
    if ext in ARCHIVE_EXT:
        return "archive"
    return "other"
