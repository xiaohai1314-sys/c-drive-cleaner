#include "core/timeutil.h"

#include <windows.h>

#include <cstdio>

namespace {
constexpr uint64_t kWindowsToUnixSeconds = 11644473600ULL;

LocalTimeParts Convert(uint64_t unixSeconds) {
    LocalTimeParts parts;
    const uint64_t fileTimeValue =
        (unixSeconds + kWindowsToUnixSeconds) * 10000000ULL;
    FILETIME fileTime;
    fileTime.dwLowDateTime = static_cast<DWORD>(fileTimeValue & 0xFFFFFFFFULL);
    fileTime.dwHighDateTime = static_cast<DWORD>(fileTimeValue >> 32);
    FILETIME localTime;
    SYSTEMTIME systemTime;
    if (FileTimeToLocalFileTime(&fileTime, &localTime) == 0 ||
        FileTimeToSystemTime(&localTime, &systemTime) == 0) {
        return parts;
    }
    parts.year = systemTime.wYear;
    parts.month = systemTime.wMonth;
    parts.day = systemTime.wDay;
    parts.hour = systemTime.wHour;
    parts.minute = systemTime.wMinute;
    parts.second = systemTime.wSecond;
    return parts;
}

std::string Format(const char* pattern, const LocalTimeParts& parts) {
    char buffer[64];
    std::snprintf(buffer, sizeof(buffer), pattern, parts.year, parts.month,
                  parts.day, parts.hour, parts.minute, parts.second);
    return buffer;
}
}  // namespace

uint64_t NowUnixSeconds() {
    FILETIME fileTime;
    GetSystemTimeAsFileTime(&fileTime);
    const uint64_t value =
        (static_cast<uint64_t>(fileTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(fileTime.dwLowDateTime);
    return value / 10000000ULL - kWindowsToUnixSeconds;
}

uint64_t NowMs() {
    FILETIME fileTime;
    GetSystemTimeAsFileTime(&fileTime);
    const uint64_t value =
        (static_cast<uint64_t>(fileTime.dwHighDateTime) << 32) |
        static_cast<uint64_t>(fileTime.dwLowDateTime);
    return value / 10000ULL - kWindowsToUnixSeconds * 1000ULL;
}

LocalTimeParts LocalTimeFromUnix(uint64_t seconds) { return Convert(seconds); }

std::string MonthLabel(uint64_t seconds) {
    const LocalTimeParts parts = Convert(seconds);
    if (parts.year == 0) {
        return "1970-01";
    }
    char buffer[16];
    std::snprintf(buffer, sizeof(buffer), "%04d-%02d", parts.year, parts.month);
    return buffer;
}

std::string Stamp() {
    return Format("%04d%02d%02d_%02d%02d%02d", Convert(NowUnixSeconds()));
}

std::string HumanStamp(uint64_t seconds) {
    return Format("%04d-%02d-%02d %02d:%02d:%02d", Convert(seconds));
}
