#include "core/disk.h"

#include <windows.h>
#include <winioctl.h>

#include <cctype>
#include <string>

#include "util/utf.h"
#include "util/win32.h"

namespace {

std::optional<wchar_t> DriveLetter(const std::string& drive) {
    size_t index = 0;
    while (index < drive.size() && (drive[index] == '\\' || drive[index] == '/')) {
        ++index;
    }
    if (index >= drive.size()) {
        return std::nullopt;
    }
    const unsigned char ch = static_cast<unsigned char>(drive[index]);
    if (std::isalpha(ch) == 0) {
        return std::nullopt;
    }
    return static_cast<wchar_t>(std::toupper(ch));
}

UniqueHandle OpenVolume(const wchar_t letter) {
    const std::wstring name = std::wstring(L"\\\\.\\") + letter + L":";
    return UniqueHandle(CreateFileW(name.c_str(), 0,
                                    FILE_SHARE_READ | FILE_SHARE_WRITE |
                                        FILE_SHARE_DELETE,
                                    nullptr, OPEN_EXISTING, 0, nullptr));
}

}  // namespace

namespace disk {

std::optional<bool> IsSsd(const std::string& drive) {
    const std::optional<wchar_t> letter = DriveLetter(drive);
    if (!letter.has_value()) {
        return std::nullopt;
    }
    UniqueHandle handle = OpenVolume(*letter);
    if (!handle) {
        return std::nullopt;
    }
    STORAGE_PROPERTY_QUERY query = {};
    query.PropertyId = StorageDeviceSeekPenaltyProperty;
    query.QueryType = PropertyStandardQuery;
    DEVICE_SEEK_PENALTY_DESCRIPTOR descriptor = {};
    DWORD returned = 0;
    if (DeviceIoControl(handle.get(), IOCTL_STORAGE_QUERY_PROPERTY, &query,
                        sizeof(query), &descriptor, sizeof(descriptor),
                        &returned, nullptr) == 0) {
        return std::nullopt;
    }
    return descriptor.IncursSeekPenalty == 0;
}

std::optional<std::pair<uint32_t, uint32_t>> PhysicalDiskId(
    const std::string& drive) {
    const std::optional<wchar_t> letter = DriveLetter(drive);
    if (!letter.has_value()) {
        return std::nullopt;
    }
    UniqueHandle handle = OpenVolume(*letter);
    if (!handle) {
        return std::nullopt;
    }
    STORAGE_DEVICE_NUMBER info = {};
    DWORD returned = 0;
    if (DeviceIoControl(handle.get(), IOCTL_STORAGE_GET_DEVICE_NUMBER, nullptr, 0,
                        &info, sizeof(info), &returned, nullptr) == 0) {
        return std::nullopt;
    }
    return std::make_pair(static_cast<uint32_t>(info.DeviceType),
                          static_cast<uint32_t>(info.DeviceNumber));
}

std::vector<std::string> FixedDrives() {
    std::vector<std::string> drives;
    const DWORD mask = GetLogicalDrives();
    for (uint32_t index = 0; index < 26; ++index) {
        if ((mask & (1u << index)) == 0) {
            continue;
        }
        const std::wstring root =
            std::wstring(1, static_cast<wchar_t>(L'A' + index)) + L":\\";
        if (GetDriveTypeW(root.c_str()) == DRIVE_FIXED) {
            drives.push_back(WideToUtf8(root));
        }
    }
    if (drives.empty()) {
        drives.push_back("C:\\");
    }
    return drives;
}

std::optional<DriveSpace> FreeSpace(const std::string& drive) {
    ULARGE_INTEGER freeToCaller = {};
    ULARGE_INTEGER total = {};
    ULARGE_INTEGER totalFree = {};
    if (GetDiskFreeSpaceExW(Utf8ToWide(drive).c_str(), &freeToCaller, &total,
                            &totalFree) == 0) {
        return std::nullopt;
    }
    DriveSpace space;
    space.freeBytes = freeToCaller.QuadPart;
    space.totalBytes = total.QuadPart;
    return space;
}

DiskKey KeyForPath(const std::string& path,
                   std::unordered_map<char, DiskKey>& cache) {
    char letter = 'C';
    for (const char ch : path) {
        if (ch != '\\' && ch != '/') {
            if (std::isalpha(static_cast<unsigned char>(ch)) != 0) {
                letter = static_cast<char>(
                    std::toupper(static_cast<unsigned char>(ch)));
            }
            break;
        }
    }
    const auto found = cache.find(letter);
    if (found != cache.end()) {
        return found->second;
    }
    const std::string drive = std::string(1, letter) + ":\\";
    DiskKey key;
    const std::optional<bool> ssd = IsSsd(drive);
    if (!ssd.has_value() || *ssd) {
        key.kind = DiskKeyKind::Ssd;
    } else {
        const auto physical = PhysicalDiskId(drive);
        if (physical.has_value()) {
            key.kind = DiskKeyKind::HddPhysical;
            key.deviceType = physical->first;
            key.deviceNumber = physical->second;
        } else {
            key.kind = DiskKeyKind::HddDrive;
            key.letter = letter;
        }
    }
    cache[letter] = key;
    return key;
}

}  // namespace disk
