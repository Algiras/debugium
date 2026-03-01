/**
 * Debugium C test target.
 * Compile: cc -g -O0 tests/target_c.c -o /tmp/target_c
 * Debug:   debugium launch /tmp/target_c --adapter lldb --breakpoint /abs/path/tests/target_c.c:20
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

int fibonacci(int n) {
    if (n <= 1) return n;
    int a = 0, b = 1;
    for (int i = 2; i <= n; i++) {
        int tmp = a + b;
        a = b;
        b = tmp;
    }
    return b;
}

int main(int argc, char *argv[]) {
    int count = 10;
    int *fibs = malloc(count * sizeof(int));

    for (int i = 0; i < count; i++) {
        fibs[i] = fibonacci(i);
        printf("fib(%d) = %d\n", i, fibs[i]);
    }

    // Sum
    int sum = 0;
    for (int i = 0; i < count; i++) {
        sum += fibs[i];
    }
    printf("Sum: %d\n", sum);

    free(fibs);
    return 0;
}
