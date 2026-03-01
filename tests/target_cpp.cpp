/**
 * Debugium C++ test target.
 * Compile: c++ -std=c++17 -g -O0 tests/target_cpp.cpp -o /tmp/target_cpp
 * Debug:   debugium launch /tmp/target_cpp --adapter lldb --breakpoint /abs/path/tests/target_cpp.cpp:35
 */
#include <iostream>
#include <vector>
#include <string>
#include <numeric>
#include <algorithm>

struct Session {
    std::string id;
    std::string adapter;
    bool paused;
    int stack_depth;
};

std::vector<int> fibonacci_seq(int n) {
    std::vector<int> seq = {0, 1};
    for (int i = 2; i < n; i++) {
        seq.push_back(seq[i-1] + seq[i-2]);
    }
    return seq;
}

int main() {
    // Breakpoint target 1: vector + algorithm
    auto fibs = fibonacci_seq(10);
    int sum = std::accumulate(fibs.begin(), fibs.end(), 0);
    std::cout << "Fibonacci sum: " << sum << std::endl;

    // Breakpoint target 2: structs
    std::vector<Session> sessions = {
        {"py-1", "python", true, 3},
        {"js-1", "node", false, 0},
        {"rs-1", "lldb", true, 5},
    };

    for (const auto& s : sessions) {
        std::cout << s.id << ": " << s.adapter
                  << (s.paused ? " [paused]" : " [running]")
                  << " depth=" << s.stack_depth << std::endl;
    }

    // Breakpoint target 3: lambda + transform
    std::vector<std::string> labels;
    std::transform(fibs.begin(), fibs.end(), std::back_inserter(labels),
        [](int n) -> std::string {
            if (n % 15 == 0) return "fizzbuzz";
            if (n % 3 == 0) return "fizz";
            if (n % 5 == 0) return "buzz";
            return std::to_string(n);
        });

    for (const auto& l : labels) {
        std::cout << l << " ";
    }
    std::cout << std::endl;

    return 0;
}
