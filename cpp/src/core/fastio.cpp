#include "core/fastio.h"

#include <windows.h>

#include <algorithm>
#include <array>
#include <cctype>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <unordered_map>

#include "core/mft.h"
#include "util/utf.h"

namespace {

constexpr DWORD kShareAll =
    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
constexpr size_t kHeadSize = 4096;

char UpperLetter(const std::string& path) {
    for (const char ch : path) {
        if (ch != '\\' && ch != '/') {
            return static_cast<char>(
                std::toupper(static_cast<unsigned char>(ch)));
        }
    }
    return 'C';
}

std::optional<std::pair<uint32_t, uint64_t>> IdentityOf(HANDLE handle) {
    BY_HANDLE_FILE_INFORMATION info = {};
    if (GetFileInformationByHandle(handle, &info) == 0) {
        return std::nullopt;
    }
    const uint64_t index =
        (static_cast<uint64_t>(info.nFileIndexHigh) << 32) |
        static_cast<uint64_t>(info.nFileIndexLow);
    if (info.nNumberOfLinks > 1 && index != 0) {
        return std::make_pair(static_cast<uint32_t>(info.dwVolumeSerialNumber),
                              index);
    }
    return std::nullopt;
}

UniqueHandle OpenRawHandleByIdOrPath(HANDLE hint, uint64_t frn,
                                     const std::string& path, DWORD flags) {
    if (frn != 0 && hint != nullptr) {
        FILE_ID_DESCRIPTOR descriptor = {};
        descriptor.dwSize = sizeof(descriptor);
        descriptor.Type = FileIdType;
        descriptor.FileId.QuadPart = static_cast<LONGLONG>(frn);
        HANDLE handle =
            OpenFileById(hint, &descriptor, GENERIC_READ, kShareAll, nullptr,
                         flags);
        if (handle != nullptr && handle != INVALID_HANDLE_VALUE) {
            return UniqueHandle(handle);
        }
    }
    HANDLE handle = CreateFileW(Utf8ToWide(path).c_str(), GENERIC_READ,
                                kShareAll, nullptr, OPEN_EXISTING, flags,
                                nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return UniqueHandle();
    }
    return UniqueHandle(handle);
}

struct IocpSlot {
    OVERLAPPED overlapped = {};
    HANDLE handle = nullptr;
    size_t itemIdx = 0;
    size_t want = 0;
    bool active = false;
    std::array<uint8_t, kHeadSize> buffer = {};
};

}  // namespace

namespace fastio {

UniqueHandle OpenDriveHint(char driveLetter) {
    const std::wstring root =
        std::wstring(1, static_cast<wchar_t>(std::toupper(
                            static_cast<unsigned char>(driveLetter)))) +
        L":\\";
    HANDLE handle = CreateFileW(root.c_str(), 0, kShareAll, nullptr,
                                OPEN_EXISTING, FILE_FLAG_BACKUP_SEMANTICS,
                                nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return UniqueHandle();
    }
    return UniqueHandle(handle);
}

std::optional<OpenedFile> OpenCheckedFileFast(HANDLE hint, uint64_t frn,
                                              const std::string& path,
                                              DWORD flags) {
    UniqueHandle file = OpenRawHandleByIdOrPath(hint, frn, path, flags);
    if (!file) {
        return std::nullopt;
    }
    OpenedFile result;
    BY_HANDLE_FILE_INFORMATION info = {};
    if (GetFileInformationByHandle(file.get(), &info) != 0) {
        if ((info.dwFileAttributes & mft::kSkipFileAttrs) != 0) {
            return std::nullopt;
        }
        const uint64_t index =
            (static_cast<uint64_t>(info.nFileIndexHigh) << 32) |
            static_cast<uint64_t>(info.nFileIndexLow);
        if (info.nNumberOfLinks > 1 && index != 0) {
            result.fileId = std::make_pair(
                static_cast<uint32_t>(info.dwVolumeSerialNumber), index);
        }
        result.size = (static_cast<uint64_t>(info.nFileSizeHigh) << 32) |
                      static_cast<uint64_t>(info.nFileSizeLow);
    } else {
        LARGE_INTEGER size = {};
        if (GetFileSizeEx(file.get(), &size) != 0) {
            result.size = static_cast<uint64_t>(size.QuadPart);
        }
    }
    result.handle = std::move(file);
    return result;
}

std::optional<std::pair<uint32_t, uint64_t>> QueryFileIdentity(
    const std::string& path) {
    HANDLE handle = CreateFileW(Utf8ToWide(path).c_str(), FILE_READ_ATTRIBUTES,
                                kShareAll, nullptr, OPEN_EXISTING,
                                FILE_FLAG_BACKUP_SEMANTICS, nullptr);
    if (handle == INVALID_HANDLE_VALUE) {
        return std::nullopt;
    }
    UniqueHandle guard(handle);
    return IdentityOf(guard.get());
}

std::vector<std::pair<size_t, Digest>> CoalescedRawVolumeHeads(
    char driveLetter, std::vector<RawHeadTarget> targets, bool isSsd,
    const std::atomic<bool>& cancel, const std::function<void()>& onTick) {
    std::vector<std::pair<size_t, Digest>> out;
    if (targets.empty()) {
        return out;
    }
    const std::wstring volumePath =
        std::wstring(L"\\\\.\\") +
        static_cast<wchar_t>(
            std::toupper(static_cast<unsigned char>(driveLetter))) +
        L":";
    HANDLE raw = CreateFileW(volumePath.c_str(), GENERIC_READ,
                             FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr,
                             OPEN_EXISTING,
                             isSsd ? FILE_FLAG_RANDOM_ACCESS
                                   : FILE_FLAG_SEQUENTIAL_SCAN,
                             nullptr);
    if (raw == INVALID_HANDLE_VALUE) {
        return out;
    }
    UniqueHandle volume(raw);

    std::sort(targets.begin(), targets.end(),
              [](const RawHeadTarget& left, const RawHeadTarget& right) {
                  return left.offset < right.offset;
              });

    // 机械盘上每次读都要寻道，把邻近文件的头部合并成一次读能显著减少寻道次数；
    // 固态盘寻道几乎免费，窗口小一点避免多读无用数据。
    const uint64_t maxGap = isSsd ? 64 * 1024 : 1024 * 1024;
    const uint64_t maxSpan = isSsd ? 1024 * 1024 : 4 * 1024 * 1024;
    std::vector<uint8_t> buffer(static_cast<size_t>(maxSpan));
    out.reserve(targets.size());
    size_t readCount = 0;

    size_t index = 0;
    while (index < targets.size()) {
        if (cancel.load()) {
            break;
        }
        const uint64_t startOffset = targets[index].offset;
        uint64_t endOffset = startOffset + kHeadSize;
        size_t next = index + 1;
        while (next < targets.size()) {
            const uint64_t nextOffset = targets[next].offset;
            const uint64_t nextEnd = nextOffset + kHeadSize;
            if (nextOffset <= endOffset + maxGap &&
                nextEnd - startOffset <= maxSpan) {
                if (nextEnd > endOffset) {
                    endOffset = nextEnd;
                }
                ++next;
            } else {
                break;
            }
        }

        const size_t spanLength = static_cast<size_t>(endOffset - startOffset);
        ++readCount;
        LARGE_INTEGER distance;
        distance.QuadPart = static_cast<LONGLONG>(startOffset);
        bool ok = SetFilePointerEx(volume.get(), distance, nullptr, FILE_BEGIN) != 0;
        if (ok) {
            DWORD read = 0;
            ok = ReadFile(volume.get(), buffer.data(),
                          static_cast<DWORD>(spanLength), &read, nullptr) != 0 &&
                 read == spanLength;
        }
        if (ok) {
            for (size_t item = index; item < next; ++item) {
                const size_t relative =
                    static_cast<size_t>(targets[item].offset - startOffset);
                const size_t end = relative + targets[item].want;
                if (end <= spanLength) {
                    out.emplace_back(
                        targets[item].idx,
                        Blake3Hash(buffer.data() + relative,
                                   targets[item].want));
                    onTick();
                }
            }
        }
        index = next;
    }
    if (std::getenv("CDC_TRACE") != nullptr) {
        std::fprintf(stderr, "raw[%c]: %zu 个文件 -> %zu 次读%s\n", driveLetter,
                     targets.size(), readCount, isSsd ? "（固态）" : "（机械）");
    }
    return out;
}

std::vector<IocpHeadResult> IocpReadHeads(
    std::vector<IocpHeadRequest> requests, size_t concurrency,
    const std::atomic<bool>& cancel, const std::function<void()>& onTick) {
    std::vector<IocpHeadResult> results;
    if (requests.empty()) {
        return results;
    }
    HANDLE portRaw =
        CreateIoCompletionPort(INVALID_HANDLE_VALUE, nullptr, 0, 0);
    if (portRaw == nullptr) {
        return results;
    }
    UniqueHandle port(portRaw);

    std::unordered_map<char, UniqueHandle> hints;
    for (const IocpHeadRequest& request : requests) {
        if (request.path.empty() || request.frn == 0) {
            continue;
        }
        const char letter = UpperLetter(request.path);
        if (hints.find(letter) == hints.end()) {
            UniqueHandle hint = OpenDriveHint(letter);
            if (hint) {
                hints.emplace(letter, std::move(hint));
            }
        }
    }

    const size_t poolSize =
        std::min<size_t>(std::min<size_t>(std::max<size_t>(concurrency, 1), 64),
                         requests.size());
    std::vector<IocpSlot> slots(poolSize);
    std::vector<size_t> freeSlots;
    freeSlots.reserve(poolSize);
    for (size_t index = poolSize; index > 0; --index) {
        freeSlots.push_back(index - 1);
    }
    results.reserve(requests.size());
    size_t nextRequest = 0;
    size_t inFlight = 0;

    auto submitNext = [&](size_t slotIndex) -> bool {
        IocpSlot& slot = slots[slotIndex];
        while (nextRequest < requests.size()) {
            if (cancel.load()) {
                return false;
            }
            IocpHeadRequest& request = requests[nextRequest];
            const char letter = UpperLetter(request.path);
            const auto hint = hints.find(letter);
            HANDLE hintHandle =
                hint == hints.end() ? nullptr : hint->second.get();
            UniqueHandle handle = OpenRawHandleByIdOrPath(
                hintHandle, request.frn, request.path,
                FILE_FLAG_OVERLAPPED | FILE_FLAG_RANDOM_ACCESS);
            if (!handle) {
                ++nextRequest;
                onTick();
                continue;
            }

            if (request.frn == 0) {
                BY_HANDLE_FILE_INFORMATION info = {};
                if (GetFileInformationByHandle(handle.get(), &info) != 0 &&
                    (info.dwFileAttributes & mft::kSkipFileAttrs) != 0) {
                    ++nextRequest;
                    onTick();
                    continue;
                }
            }

            HANDLE bound = CreateIoCompletionPort(handle.get(), port.get(),
                                                  slotIndex, 0);
            if (bound == nullptr) {
                ++nextRequest;
                onTick();
                continue;
            }

            const size_t want = std::min(request.want, kHeadSize);
            if (want == 0) {
                results.push_back(
                    IocpHeadResult{request.idx, Blake3Hash(nullptr, 0),
                                   std::nullopt});
                ++nextRequest;
                onTick();
                continue;
            }

            slot.overlapped = {};
            slot.handle = handle.release();
            slot.itemIdx = request.idx;
            slot.want = want;
            slot.active = true;
            ++nextRequest;

            DWORD transferred = 0;
            const BOOL ok =
                ReadFile(slot.handle, slot.buffer.data(),
                         static_cast<DWORD>(want), &transferred,
                         &slot.overlapped);
            if (!ok && GetLastError() != ERROR_IO_PENDING) {
                slot.active = false;
                CloseHandle(slot.handle);
                slot.handle = nullptr;
                onTick();
                continue;
            }
            return true;
        }
        return false;
    };

    while (!freeSlots.empty()) {
        const size_t slotIndex = freeSlots.back();
        if (submitNext(slotIndex)) {
            freeSlots.pop_back();
            ++inFlight;
        } else {
            break;
        }
    }

    while (inFlight > 0) {
        DWORD transferred = 0;
        ULONG_PTR completionKey = 0;
        LPOVERLAPPED overlapped = nullptr;
        const BOOL ok = GetQueuedCompletionStatus(
            port.get(), &transferred, &completionKey, &overlapped, INFINITE);
        if (overlapped == nullptr) {
            break;
        }
        const size_t slotIndex = static_cast<size_t>(completionKey);
        IocpSlot& slot = slots[slotIndex];
        if (ok != 0 && transferred > 0) {
            const size_t length =
                std::min<size_t>(transferred, slot.want);
            IocpHeadResult result;
            result.idx = slot.itemIdx;
            result.digest = Blake3Hash(slot.buffer.data(), length);
            result.fileId = IdentityOf(slot.handle);
            results.push_back(std::move(result));
        }
        if (slot.handle != nullptr) {
            CloseHandle(slot.handle);
            slot.handle = nullptr;
        }
        slot.active = false;
        --inFlight;
        onTick();

        if (!cancel.load() && submitNext(slotIndex)) {
            ++inFlight;
        } else {
            freeSlots.push_back(slotIndex);
            if (cancel.load()) {
                for (IocpSlot& other : slots) {
                    if (other.active && other.handle != nullptr) {
                        CancelIoEx(other.handle, nullptr);
                    }
                }
            }
        }
    }

    return results;
}

}  // namespace fastio
