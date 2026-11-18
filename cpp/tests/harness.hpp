#pragma once

#include <cstdio>
#include <cstdlib>
#include <string>
#include <vector>

struct TestCase {
    const char* name;
    void (*fn)();
};

std::vector<TestCase>& AllTests();

#define TEST(name)                                                          \
    static void name();                                                     \
    static const bool name##_registered =                                   \
        (AllTests().push_back({#name, name}), true);                        \
    static void name()

#define CHECK(condition)                                                    \
    do {                                                                    \
        if (!(condition)) {                                                 \
            std::fprintf(stderr, "CHECK failed at %s:%d: %s\n", __FILE__,   \
                         __LINE__, #condition);                             \
            std::exit(1);                                                   \
        }                                                                   \
    } while (0)

#define CHECK_EQ(actual, expected)                                          \
    do {                                                                    \
        const auto& actualValue = (actual);                                 \
        const auto& expectedValue = (expected);                             \
        if (!(actualValue == expectedValue)) {                              \
            std::fprintf(stderr, "CHECK_EQ failed at %s:%d: %s != %s\n",    \
                         __FILE__, __LINE__, #actual, #expected);           \
            std::exit(1);                                                   \
        }                                                                   \
    } while (0)
