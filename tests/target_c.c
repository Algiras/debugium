/**
 * Debugium C test target — mirrors target_python.py for cross-language testing.
 * Compile: cc -g -O0 tests/target_c.c -o /tmp/debugium_target_c
 * Run:     debugium launch /tmp/debugium_target_c --config examples/c-cpp.dap.json \
 *            --breakpoint $(pwd)/tests/target_c.c:36
 */
#include <stdio.h>
#include <string.h>

int fibonacci(int n, int *out) {
    if (n < 1) return 0;
    out[0] = 0;
    if (n < 2) return 1;
    out[1] = 1;
    for (int i = 2; i < n; i++) {
        out[i] = out[i-1] + out[i-2];
    }
    return n;
}

const char *classify(int value) {
    if (value % 15 == 0) return "fizzbuzz";
    if (value % 3 == 0) return "fizz";
    if (value % 5 == 0) return "buzz";
    return "";
}

int main(void) {
    int fibs[10];
    int count = fibonacci(10, fibs);

    printf("Fibonacci(%d):", count);
    for (int i = 0; i < count; i++) {
        const char *label = classify(fibs[i]);
        printf(" %d(%s)", fibs[i], strlen(label) ? label : "-");
    }
    printf("\n");

    int counter = 10;
    int steps[] = {1, 2, 3, 5, 8, 13};
    for (int i = 0; i < 6; i++) {
        counter += steps[i];
        const char *label = classify(counter);
        printf("  step=%d -> counter=%d (%s)\n",
               steps[i], counter, strlen(label) ? label : "-");
    }

    printf("Final counter: %d\n", counter);
    return 0;
}
