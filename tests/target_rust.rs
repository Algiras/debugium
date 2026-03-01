/// Debugium Rust test target.
/// Compile: rustc -g tests/target_rust.rs -o /tmp/target_rust
/// Debug:   debugium launch /tmp/target_rust --config examples/rust.dap.json \
///            --breakpoint "$(pwd)/tests/target_rust.rs:20"

fn fibonacci(n: u32) -> Vec<u64> {
    let mut fibs = vec![0u64, 1];
    for i in 2..n as usize {
        let next = fibs[i - 1] + fibs[i - 2];
        fibs.push(next);
    }
    fibs
}

fn main() {
    let count = 10;
    let fibs = fibonacci(count);

    // breakpoint target — line 20
    let total: u64 = fibs.iter().sum();
    println!("First {count} Fibonacci numbers: {fibs:?}");
    println!("Sum: {total}");

    for (i, f) in fibs.iter().enumerate() {
        println!("  fib[{i}] = {f}");
    }
}
