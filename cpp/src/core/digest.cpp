#include "core/digest.h"

#include "blake3.h"

Digest Blake3Hash(const void* data, size_t length) {
    blake3_hasher hasher;
    blake3_hasher_init(&hasher);
    if (length != 0) {
        blake3_hasher_update(&hasher, data, length);
    }
    Digest digest{};
    blake3_hasher_finalize(&hasher, digest.data(), digest.size());
    return digest;
}
