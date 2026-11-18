#pragma once

#include <cstdint>
#include <optional>
#include <string>
#include <unordered_map>
#include <utility>
#include <vector>

namespace disk {

/// 判断盘符所在卷是否固态盘（没有寻道开销）。判断不了返回 nullopt。
std::optional<bool> IsSsd(const std::string& drive);

/// 盘符对应的物理磁盘标识 (device_type, device_number)。
std::optional<std::pair<uint32_t, uint32_t>> PhysicalDiskId(
    const std::string& drive);

/// 只返回固定盘；没有时回退 ["C:\\"]。
std::vector<std::string> FixedDrives();

struct DriveSpace {
    uint64_t freeBytes = 0;
    uint64_t totalBytes = 0;
};

/// 盘符可用空间与总容量（对应 Rust 的 `get_drive_free_space`）。
std::optional<DriveSpace> FreeSpace(const std::string& drive);

enum class DiskKeyKind { Ssd, HddPhysical, HddDrive };

struct DiskKey {
    DiskKeyKind kind = DiskKeyKind::Ssd;
    uint32_t deviceType = 0;
    uint32_t deviceNumber = 0;
    char letter = 'C';

    bool operator==(const DiskKey& other) const {
        if (kind != other.kind) {
            return false;
        }
        switch (kind) {
            case DiskKeyKind::Ssd:
                return true;
            case DiskKeyKind::HddPhysical:
                return deviceType == other.deviceType &&
                       deviceNumber == other.deviceNumber;
            case DiskKeyKind::HddDrive:
                return letter == other.letter;
        }
        return false;
    }
};

struct DiskKeyHash {
    size_t operator()(const DiskKey& key) const {
        size_t value = static_cast<size_t>(key.kind) * 1315423911u;
        switch (key.kind) {
            case DiskKeyKind::Ssd:
                break;
            case DiskKeyKind::HddPhysical:
                value ^= static_cast<size_t>(key.deviceType) * 2654435761u;
                value ^= static_cast<size_t>(key.deviceNumber) * 40503u;
                break;
            case DiskKeyKind::HddDrive:
                value ^= static_cast<size_t>(static_cast<unsigned char>(key.letter));
                break;
        }
        return value;
    }
};

DiskKey KeyForPath(const std::string& path,
                   std::unordered_map<char, DiskKey>& cache);

}  // namespace disk
