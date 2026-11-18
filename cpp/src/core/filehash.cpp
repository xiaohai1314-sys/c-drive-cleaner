#include "core/filehash.h"

#include <windows.h>

#include <algorithm>
#include <cctype>
#include <condition_variable>
#include <cstring>
#include <deque>
#include <mutex>
#include <thread>
#include <unordered_map>
#include <vector>

#include "blake3.h"
#include "core/fastio.h"
#include "util/win32.h"

namespace {

constexpr uint64_t kSampleChunk = 16 * 1024;
constexpr uint64_t kStrongChunk = 256 * 1024;
constexpr uint64_t kProbeChunk = 64 * 1024;
constexpr uint64_t kDoubleBufThreshold = 16 * 1024 * 1024;
constexpr size_t kFullBufSize = 1024 * 1024;
constexpr size_t kDoubleBufSize = 2 * 1024 * 1024;

const std::atomic<bool> kNeverCancel{false};

struct Opened {
    fastio::OpenedFile file;
    uint64_t size = 0;
};

std::optional<Opened> OpenCheckedFileWithSize(const std::string& path,
                                              uint64_t frn, DWORD flags) {
    if (frn == 0) {
        std::optional<fastio::OpenedFile> opened =
            fastio::OpenCheckedFileFast(nullptr, 0, path, flags);
        if (!opened.has_value()) {
            return std::nullopt;
        }
        Opened result;
        result.size = opened->size;
        result.file = std::move(*opened);
        return result;
    }
    thread_local std::unordered_map<char, UniqueHandle> hints;
    char letter = 'C';
    for (const char ch : path) {
        if (ch != '\\' && ch != '/') {
            letter = static_cast<char>(
                std::toupper(static_cast<unsigned char>(ch)));
            break;
        }
    }
    auto found = hints.find(letter);
    if (found == hints.end()) {
        found = hints.emplace(letter, fastio::OpenDriveHint(letter)).first;
    }
    HANDLE hint = found->second ? found->second.get() : nullptr;
    std::optional<fastio::OpenedFile> opened =
        fastio::OpenCheckedFileFast(hint, frn, path, flags);
    if (!opened.has_value()) {
        return std::nullopt;
    }
    Opened result;
    result.size = opened->size;
    result.file = std::move(*opened);
    return result;
}

bool HashChunk(HANDLE handle, std::vector<uint8_t>& buffer, uint64_t offset,
               uint64_t length, blake3_hasher& hasher) {
    LARGE_INTEGER distance;
    distance.QuadPart = static_cast<LONGLONG>(offset);
    if (SetFilePointerEx(handle, distance, nullptr, FILE_BEGIN) == 0) {
        return false;
    }
    uint64_t remaining = length;
    while (remaining > 0) {
        const DWORD want = static_cast<DWORD>(
            std::min<uint64_t>(remaining, buffer.size()));
        DWORD read = 0;
        if (ReadFile(handle, buffer.data(), want, &read, nullptr) == 0) {
            return false;
        }
        if (read == 0) {
            break;
        }
        blake3_hasher_update(&hasher, buffer.data(), read);
        remaining -= read;
    }
    return true;
}

Digest Finish(blake3_hasher& hasher) {
    Digest digest{};
    blake3_hasher_finalize(&hasher, digest.data(), digest.size());
    return digest;
}

std::optional<filehash::Hashed> TripleHash(const std::string& path,
                                           uint64_t frn, uint64_t size,
                                           uint64_t chunk, uint64_t fullThreshold) {
    if (size <= fullThreshold) {
        return filehash::FullHash(path, frn, kNeverCancel);
    }
    std::optional<Opened> opened =
        OpenCheckedFileWithSize(path, frn, FILE_FLAG_RANDOM_ACCESS);
    if (!opened.has_value()) {
        return std::nullopt;
    }
    thread_local std::vector<uint8_t> buffer;
    if (buffer.size() < chunk) {
        buffer.resize(chunk);
    }
    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    if (!HashChunk(opened->file.handle.get(), buffer, 0, chunk, hasher) ||
        !HashChunk(opened->file.handle.get(), buffer, size / 2, chunk,
                   hasher) ||
        !HashChunk(opened->file.handle.get(), buffer, size - chunk, chunk,
                   hasher)) {
        return std::nullopt;
    }
    filehash::Hashed result;
    result.digest = Finish(hasher);
    result.fileId = opened->file.fileId;
    return result;
}

/// 有界阻塞队列：读线程与哈希线程之间的双缓冲交接。
class BlockQueue {
public:
    explicit BlockQueue(size_t capacity) : capacity_(capacity) {}
    ~BlockQueue() { Close(); }

    BlockQueue(const BlockQueue&) = delete;
    BlockQueue& operator=(const BlockQueue&) = delete;

    bool Push(std::vector<uint8_t> block) {
        std::unique_lock<std::mutex> lock(mutex_);
        notFull_.wait(lock, [this] { return closed_ || items_.size() < capacity_; });
        if (closed_) {
            return false;
        }
        items_.push_back(std::move(block));
        notEmpty_.notify_one();
        return true;
    }

    bool Pop(std::vector<uint8_t>& out) {
        std::unique_lock<std::mutex> lock(mutex_);
        notEmpty_.wait(lock, [this] { return closed_ || !items_.empty(); });
        if (items_.empty()) {
            return false;
        }
        out = std::move(items_.front());
        items_.pop_front();
        notFull_.notify_one();
        return true;
    }

    void Close() {
        std::lock_guard<std::mutex> lock(mutex_);
        closed_ = true;
        notEmpty_.notify_all();
        notFull_.notify_all();
    }

private:
    size_t capacity_;
    std::deque<std::vector<uint8_t>> items_;
    std::mutex mutex_;
    std::condition_variable notEmpty_;
    std::condition_variable notFull_;
    bool closed_ = false;
};

std::optional<filehash::Hashed> FullHashDoubleBuffered(HANDLE handle,
                                                       uint64_t frn,
                                                       const std::atomic<bool>& cancel) {
    BlockQueue empty(2);
    BlockQueue filled(1);
    empty.Push(std::vector<uint8_t>(kDoubleBufSize));
    empty.Push(std::vector<uint8_t>(kDoubleBufSize));

    bool ok = true;
    std::thread reader([&handle, &empty, &filled, &cancel, &ok] {
        while (true) {
            std::vector<uint8_t> block;
            if (!empty.Pop(block)) {
                break;
            }
            if (cancel.load()) {
                ok = false;
                break;
            }
            size_t have = 0;
            while (have < block.size()) {
                DWORD read = 0;
                if (ReadFile(handle, block.data() + have,
                             static_cast<DWORD>(block.size() - have), &read,
                             nullptr) == 0) {
                    ok = false;
                    break;
                }
                if (read == 0) {
                    break;
                }
                have += read;
            }
            if (!ok || have == 0) {
                break;
            }
            block.resize(have);
            if (!filled.Push(std::move(block))) {
                break;
            }
        }
        filled.Close();
    });

    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    std::vector<uint8_t> block;
    while (filled.Pop(block)) {
        if (cancel.load()) {
            ok = false;
            break;
        }
        blake3_hasher_update(&hasher, block.data(), block.size());
        block.resize(kDoubleBufSize);
        if (!empty.Push(std::move(block))) {
            break;
        }
    }
    filled.Close();
    empty.Close();
    reader.join();
    if (!ok || cancel.load()) {
        return std::nullopt;
    }
    filehash::Hashed result;
    result.digest = Finish(hasher);
    (void)frn;
    return result;
}

}  // namespace

namespace filehash {

std::optional<Hashed> SampleHash(const std::string& path, uint64_t frn,
                                 uint64_t size) {
    return TripleHash(path, frn, size, kSampleChunk, kSampleChunk * 3);
}

std::optional<Hashed> StrongHash(const std::string& path, uint64_t frn,
                                 uint64_t size) {
    return TripleHash(path, frn, size, kStrongChunk, kStrongChunk * 3);
}

std::optional<Hashed> ProbeHash(const std::string& path, uint64_t frn,
                                uint64_t size) {
    std::optional<Opened> opened =
        OpenCheckedFileWithSize(path, frn, FILE_FLAG_RANDOM_ACCESS);
    if (!opened.has_value()) {
        return std::nullopt;
    }
    thread_local std::vector<uint8_t> buffer(kProbeChunk);
    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    const uint64_t tail = size > kProbeChunk ? size - kProbeChunk : 0;
    const uint64_t offset1 = std::min(size / 4, tail);
    const uint64_t offset2 = std::min(size * 3 / 4, tail);
    const uint64_t length = std::min(kProbeChunk, size);
    if (!HashChunk(opened->file.handle.get(), buffer, offset1, length, hasher) ||
        !HashChunk(opened->file.handle.get(), buffer, offset2, length, hasher)) {
        return std::nullopt;
    }
    Hashed result;
    result.digest = Finish(hasher);
    result.fileId = opened->file.fileId;
    return result;
}

std::optional<Hashed> FullHash(const std::string& path, uint64_t frn,
                               const std::atomic<bool>& cancel) {
    std::optional<Opened> opened =
        OpenCheckedFileWithSize(path, frn, FILE_FLAG_SEQUENTIAL_SCAN);
    if (!opened.has_value()) {
        return std::nullopt;
    }
    HANDLE handle = opened->file.handle.get();

    if (opened->size >= kDoubleBufThreshold) {
        std::optional<Hashed> result =
            FullHashDoubleBuffered(handle, frn, cancel);
        if (result.has_value()) {
            result->fileId = opened->file.fileId;
        }
        return result;
    }

    thread_local std::vector<uint8_t> buffer(kFullBufSize);
    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    uint32_t chunks = 0;
    while (true) {
        if ((chunks & 15) == 0 && cancel.load()) {
            return std::nullopt;
        }
        DWORD read = 0;
        if (ReadFile(handle, buffer.data(), static_cast<DWORD>(buffer.size()),
                     &read, nullptr) == 0) {
            return std::nullopt;
        }
        if (read == 0) {
            break;
        }
        blake3_hasher_update(&hasher, buffer.data(), read);
        ++chunks;
    }
    Hashed result;
    result.digest = Finish(hasher);
    result.fileId = opened->file.fileId;
    return result;
}

std::optional<SeqSampleFull> ReadHddSeqSampleAndFull(const std::string& path,
                                                     uint64_t frn,
                                                     uint64_t size,
                                                     uint64_t chunkLen) {
    std::optional<Opened> opened =
        OpenCheckedFileWithSize(path, frn, FILE_FLAG_SEQUENTIAL_SCAN);
    if (!opened.has_value()) {
        return std::nullopt;
    }
    if (opened->size != size || opened->size > 2 * 1024 * 1024) {
        return std::nullopt;
    }
    const size_t total = static_cast<size_t>(opened->size);
    thread_local std::vector<uint8_t> buffer(2 * 1024 * 1024);
    size_t have = 0;
    while (have < total) {
        DWORD read = 0;
        if (ReadFile(opened->file.handle.get(), buffer.data() + have,
                     static_cast<DWORD>(total - have), &read, nullptr) == 0) {
            return std::nullopt;
        }
        if (read == 0) {
            break;
        }
        have += read;
    }
    if (have != total) {
        return std::nullopt;
    }
    const size_t chunk = static_cast<size_t>(chunkLen);
    SeqSampleFull result;
    result.full = Blake3Hash(buffer.data(), total);
    if (total <= chunk * 3) {
        result.sample = result.full;
    } else {
        blake3_hasher hasher;
        blake3_hasher_init(&hasher);
        blake3_hasher_update(&hasher, buffer.data(), chunk);
        blake3_hasher_update(&hasher, buffer.data() + total / 2, chunk);
        blake3_hasher_update(&hasher, buffer.data() + total - chunk, chunk);
        result.sample = Finish(hasher);
    }
    result.fileId = opened->file.fileId;
    return result;
}

}  // namespace filehash
