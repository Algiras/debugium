// tests/target_wasm.c
// Minimal C file for WebAssembly debugging (no libc needed).
// Compile: /opt/homebrew/opt/llvm/bin/clang --target=wasm32 -g -O0 -nostdlib -Wl,--no-entry -Wl,--export-all -o /tmp/target_wasm.wasm tests/target_wasm.c

int fibonacci(int n) {
    if (n <= 1) return n;
    int a = 0, b = 1;
    for (int i = 2; i <= n; i++) {
        int tmp = a + b;
        a = b;
        b = tmp;
    }
    return b;  // breakpoint target
}

int _start() {
    return fibonacci(10);
}
