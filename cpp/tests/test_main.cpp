#include "harness.hpp"

std::vector<TestCase>& AllTests() {
    static std::vector<TestCase> tests;
    return tests;
}

int main() {
    int failures = 0;
    for (const TestCase& test : AllTests()) {
        std::printf("[ RUN ] %s\n", test.name);
        test.fn();
        std::printf("[ OK  ] %s\n", test.name);
    }
    if (failures == 0) {
        std::printf("全部 %zu 个测试通过\n", AllTests().size());
    }
    return failures == 0 ? 0 : 1;
}
