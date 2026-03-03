/**
 * Debugium Node.js test target — mirrors target_python.py for cross-language testing.
 * Run via: debugium launch tests/target_node.js --config examples/node.dap.json --breakpoint $(pwd)/tests/target_node.js:30
 */

function fibonacci(n) {
    const seq = [0, 1];
    for (let i = 2; i < n; i++) {
        seq.push(seq[i - 1] + seq[i - 2]);
    }
    return seq.slice(0, n);
}

function classify(value) {
    if (value % 15 === 0) return "fizzbuzz";
    if (value % 3 === 0) return "fizz";
    if (value % 5 === 0) return "buzz";
    return String(value);
}

class Counter {
    constructor(start = 0) {
        this.value = start;
        this.history = [];
    }
    increment(by = 1) {
        this.value += by;
        this.history.push(this.value);
        return this.value;
    }
}

function main() {
    // Breakpoint target 1
    const fibs = fibonacci(10);
    const result = fibs.map(classify);
    console.log("Fibonacci classified:", result);

    // Breakpoint target 2
    const counter = new Counter(10);
    for (const step of [1, 2, 3, 5, 8, 13]) {
        counter.increment(step);
        const label = classify(counter.value);
        console.log(`  step=${step} → counter=${counter.value} (${label})`);
    }

    // Breakpoint target 3
    const data = {
        name: "debugium",
        version: [0, 1, 0],
        adapters: ["python", "node", "lldb"],
        active: true,
    };
    console.log("Final counter:", JSON.stringify(counter));
    console.log("Metadata:", JSON.stringify(data));
}

main();
