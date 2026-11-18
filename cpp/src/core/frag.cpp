#include "core/frag.h"

#include <windows.h>

#include <cctype>
#include <cmath>
#include <cstdio>
#include <optional>
#include <stdexcept>
#include <string>
#include <utility>
#include <vector>

#include "core/disk.h"
#include "core/task.h"
#include "util/env.h"
#include "util/utf.h"
#include "util/win32.h"

namespace {

std::string LowerAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    }
    return text;
}

std::string UpperAscii(std::string text) {
    for (char& ch : text) {
        ch = static_cast<char>(std::toupper(static_cast<unsigned char>(ch)));
    }
    return text;
}

bool IsSpace(char ch) {
    return std::isspace(static_cast<unsigned char>(ch)) != 0;
}

std::string TrimStart(const std::string& text) {
    size_t begin = 0;
    while (begin < text.size() && IsSpace(text[begin])) {
        ++begin;
    }
    return text.substr(begin);
}

std::string Trim(const std::string& text) {
    size_t begin = 0;
    size_t end = text.size();
    while (begin < end && IsSpace(text[begin])) {
        ++begin;
    }
    while (end > begin && IsSpace(text[end - 1])) {
        --end;
    }
    return text.substr(begin, end - begin);
}

std::string DefragPath() {
    std::string root = GetEnv("SystemRoot");
    if (root.empty()) {
        root = "C:\\Windows";
    }
    while (!root.empty() && root.back() == '\\') {
        root.pop_back();
    }
    return root + "\\System32\\defrag.exe";
}

bool ValidFixedDrive(const std::string& drive) {
    const std::string value = Trim(drive);
    if (value.size() != 3 || value[1] != ':' || value[2] != '\\' ||
        std::isalpha(static_cast<unsigned char>(value[0])) == 0) {
        return false;
    }
    for (const std::string& item : disk::FixedDrives()) {
        if (LowerAscii(item) == LowerAscii(value)) {
            return true;
        }
    }
    return false;
}

/// 取前 count 个字符（UTF-8 码点）组成的子串。
std::string WindowChars(const std::string& text, size_t count) {
    size_t chars = 0;
    for (size_t index = 0; index < text.size(); ++index) {
        if ((static_cast<unsigned char>(text[index]) & 0xC0) != 0x80) {
            if (chars == count) {
                return text.substr(0, index);
            }
            ++chars;
        }
    }
    return text;
}

std::optional<std::string> FindAfter(const std::string& text,
                                     const std::vector<std::string>& labels) {
    const std::string lower = LowerAscii(text);
    for (const std::string& label : labels) {
        const std::string needle = LowerAscii(label);
        const size_t position = lower.find(needle);
        if (position != std::string::npos) {
            return text.substr(position + needle.size());
        }
    }
    return std::nullopt;
}

std::optional<std::pair<double, size_t>> NextNumber(const std::string& text) {
    size_t index = 0;
    while (index < text.size()) {
        const unsigned char ch = static_cast<unsigned char>(text[index]);
        if (ch >= '0' && ch <= '9') {
            const size_t start = index;
            size_t end = index + 1;
            while (end < text.size()) {
                const char next = text[end];
                if ((next >= '0' && next <= '9') || next == '.' || next == ',') {
                    ++end;
                } else {
                    break;
                }
            }
            std::string raw;
            raw.reserve(end - start);
            for (size_t cursor = start; cursor < end; ++cursor) {
                if (text[cursor] != ',') {
                    raw.push_back(text[cursor]);
                }
            }
            try {
                size_t used = 0;
                const double value = std::stod(raw, &used);
                if (used > 0) {
                    return std::make_pair(value, end);
                }
            } catch (const std::exception&) {
                // 与 Rust 一致：解析失败就继续往后找。
            }
            index = end;
            continue;
        }
        ++index;
    }
    return std::nullopt;
}

std::optional<double> NumberWithUnit(const std::string& text,
                                     const std::vector<std::string>& labels,
                                     const std::string& unit) {
    const std::optional<std::string> after = FindAfter(text, labels);
    if (!after.has_value()) {
        return std::nullopt;
    }
    const std::string window = WindowChars(*after, 60);
    const std::optional<std::pair<double, size_t>> number = NextNumber(window);
    if (!number.has_value()) {
        return std::nullopt;
    }
    const std::string rest = TrimStart(window.substr(number->second));
    const std::string restUpper = UpperAscii(rest);
    const std::string unitUpper = UpperAscii(unit);
    if (restUpper.compare(0, unitUpper.size(), unitUpper) == 0) {
        return number->first;
    }
    return std::nullopt;
}

std::optional<double> SearchPercent(const std::string& text) {
    const std::vector<std::string> labels = {
        "碎片率", "碎片总计", "文件碎片百分比", "Percent file fragmentation"};
    if (const std::optional<double> value =
            NumberWithUnit(text, labels, "%");
        value.has_value()) {
        return value;
    }
    if (const std::optional<double> value =
            NumberWithUnit(text, {"fragmented", "碎片"}, "%");
        value.has_value()) {
        return value;
    }
    // 旧版输出：第一个「xxx = 12%」通常就是碎片率，跳过可用空间那一行。
    size_t begin = 0;
    while (begin <= text.size()) {
        const size_t end = text.find('\n', begin);
        const std::string line =
            text.substr(begin, end == std::string::npos ? std::string::npos
                                                        : end - begin);
        if (line.find('=') != std::string::npos &&
            line.find('%') != std::string::npos) {
            const std::string lower = LowerAscii(line);
            if (line.find("可用") == std::string::npos &&
                lower.find("free") == std::string::npos) {
                if (const std::optional<std::pair<double, size_t>> number =
                        NextNumber(line);
                    number.has_value()) {
                    return number->first;
                }
            }
        }
        if (end == std::string::npos) {
            break;
        }
        begin = end + 1;
    }
    return std::nullopt;
}

std::optional<uint64_t> SearchCount(const std::string& text) {
    const std::vector<std::string> labels = {
        "碎片文件数", "总碎片数", "碎片数", "Fragmented files",
        "Total fragments",
        // 中文版 defrag 的「文件:」段里写的是「碎片文件 = 27290」。
        "碎片文件"};
    const std::optional<std::string> after = FindAfter(text, labels);
    if (!after.has_value()) {
        return std::nullopt;
    }
    const std::string window = WindowChars(*after, 60);
    const std::optional<std::pair<double, size_t>> number = NextNumber(window);
    if (!number.has_value()) {
        return std::nullopt;
    }
    return static_cast<uint64_t>(number->first);
}

frag::ParsedReport ParseDefrag(std::string text) {
    // 全角空格会让「= 12%」这类匹配错位，统一换成半角。
    const std::string ideographic = "\xE3\x80\x80";
    size_t position = 0;
    while ((position = text.find(ideographic, position)) != std::string::npos) {
        text.replace(position, ideographic.size(), " ");
        ++position;
    }
    frag::ParsedReport parsed;
    parsed.percent = SearchPercent(text);
    parsed.spaceGb = NumberWithUnit(
        text, {"碎片空间", "文件碎片", "Total fragmented space", "碎片文件"},
        "GB");
    parsed.count = SearchCount(text);
    parsed.freeGb = NumberWithUnit(text, {"可用空间", "Free space"}, "GB");
    return parsed;
}

bool ValidUtf8(const std::string& text) {
    return IsValidUtf8(text);
}

/// defrag 的输出是控制台编码（OEM 代码页），先按 UTF-8 试，失败再按 OEM 解码。
std::string DecodeConsole(const std::string& bytes) {
    if (bytes.empty()) {
        return {};
    }
    if (ValidUtf8(bytes)) {
        return bytes;
    }
    const int length =
        MultiByteToWideChar(CP_OEMCP, 0, bytes.data(),
                            static_cast<int>(bytes.size()), nullptr, 0);
    if (length <= 0) {
        return bytes;
    }
    std::wstring wide(static_cast<size_t>(length), L'\0');
    const int written = MultiByteToWideChar(CP_OEMCP, 0, bytes.data(),
                                            static_cast<int>(bytes.size()),
                                            wide.data(), length);
    if (written <= 0) {
        return bytes;
    }
    wide.resize(static_cast<size_t>(written));
    return WideToUtf8(wide);
}

struct FragReport {
    std::string drive;
    std::optional<double> percent;
    std::optional<double> spaceGb;
    std::optional<uint64_t> count;
    std::optional<double> freeGb;
    std::optional<bool> ssd;
};

FragReport AnalyzeDrive(const std::string& drive) {
    FragReport report;
    report.drive = drive;
    if (!ValidFixedDrive(drive)) {
        return report;
    }
    report.ssd = disk::IsSsd(drive);
    const ProcessResult result =
        RunCapture(Utf8ToWide(DefragPath()), {Utf8ToWide(drive), L"/A", L"/V"});
    std::string text;
    if (result.started) {
        text = DecodeConsole(result.out);
        if (Trim(text).empty()) {
            text = DecodeConsole(result.err);
        }
    }
    const frag::ParsedReport parsed = ParseDefrag(std::move(text));
    report.percent = parsed.percent;
    report.spaceGb = parsed.spaceGb;
    report.count = parsed.count;
    report.freeGb = parsed.freeGb;
    return report;
}

Json ToJson(const FragReport& report) {
    Json out;
    out["drive"] = report.drive;
    out["percent"] = report.percent.has_value() ? Json(*report.percent)
                                                : Json(nullptr);
    out["space_gb"] = report.spaceGb.has_value() ? Json(*report.spaceGb)
                                                 : Json(nullptr);
    out["count"] = report.count.has_value() ? Json(*report.count) : Json(nullptr);
    out["free_gb"] = report.freeGb.has_value() ? Json(*report.freeGb)
                                               : Json(nullptr);
    out["ssd"] = report.ssd.has_value() ? Json(*report.ssd) : Json(nullptr);
    return out;
}

std::string FormatOneDecimal(double value) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), "%.1f", value);
    return buffer;
}

}  // namespace

namespace frag {

ParsedReport ParseReport(std::string text) {
    return ParseDefrag(std::move(text));
}

Json Analyze(const std::string& drive) {
    std::optional<task::Guard> guard;
    try {
        task::Begin("frag_analyze", "磁盘碎片分析", false, false, 0, "");
        guard.emplace("frag_analyze");
    } catch (const std::exception&) {
        // 与 Rust 的 `.ok()` 一致：拿不到任务槽位也照常分析。
    }
    if (guard.has_value()) {
        task::SetStage("frag_analyze", "分析磁盘碎片");
    }
    const FragReport report = AnalyzeDrive(drive);
    if (guard.has_value()) {
        guard->Finish("ok", "碎片率 " +
                                FormatOneDecimal(report.percent.value_or(0.0)) +
                                "%");
    }
    return ToJson(report);
}

void Optimize(const std::string& drive) {
    task::Begin("frag", "磁盘碎片优化", false, true, 0, "");
    task::Guard guard("frag");
    task::SetStage("frag", "执行磁盘优化（可能需要几分钟）");
    if (!ValidFixedDrive(drive)) {
        const std::string message = "只允许优化系统识别出的固定磁盘";
        guard.Finish("failed", message);
        throw std::runtime_error(message);
    }
    const ProcessResult result =
        RunCapture(Utf8ToWide(DefragPath()), {Utf8ToWide(drive), L"/O"});
    if (result.Success()) {
        guard.Finish("ok", "磁盘优化完成");
        return;
    }
    const std::string message = "磁盘优化失败，请检查权限或系统工具状态";
    guard.Finish("failed", message);
    throw std::runtime_error(message);
}

}  // namespace frag
