/**
 * Debugium JavaScript test target.
 * Run via:  debugium launch tests/target_node.js --adapter node --serve
 *
 * Covers: closures, async/await, generator functions, structured data.
 */

// ── Generator ──────────────────────────────────────────────────── //
function* fibonacci() {
    let [a, b] = [0, 1];
    while (true) {
        yield a;
        [a, b] = [b, a + b];
    }
}

// ── Async ──────────────────────────────────────────────────────── //
async function fetchMock(id) {
    // Simulates async work without network
    await new Promise((r) => setTimeout(r, 10));
    return { id, payload: `data-${id}`, ts: Date.now() };
}

// ── Classifier ────────────────────────────────────────────────── //
function classify(n) {
    if (n % 15 === 0) return "fizzbuzz";
    if (n % 3 === 0) return "fizz";
    if (n % 5 === 0) return "buzz";
    return String(n);
}

// ── Main ──────────────────────────────────────────────────────── //
async function main() {
    // Breakpoint target 1: generator demo
    const gen = fibonacci();
    const fibs = Array.from({ length: 10 }, () => gen.next().value);
    const classified = fibs.map(classify);
    console.log("Fibonacci:", fibs);
    console.log("Classified:", classified);

    // Breakpoint target 2: async batch
    const ids = [1, 2, 3, 4, 5];
    const results = await Promise.all(ids.map(fetchMock));
    for (const r of results) {
        const label = classify(r.id);
        console.log(`  id=${r.id} (${label}) → ${r.payload}`);
    }

    // Breakpoint target 3: structured state
    const state = {
        name: "debugium",
        adapters: ["python", "node", "lldb"],
        runs: results.length,
        success: true,
    };
    console.log("Final state:", state);
}

main().catch(console.error);
