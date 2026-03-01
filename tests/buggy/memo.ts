/**
 * Subtle memoization bug — TypeScript
 *
 * A "smart" memoization wrapper that caches function results by argument.
 * BUG: the cache key uses JSON.stringify on the *call site timestamp*,
 * so every call gets a unique key and the cache never hits.
 *
 * Hard to see by reading — looks like reasonable caching code.
 * Obvious in the debugger: watch cache.size grow, hitRate stay at 0%.
 */

export {}; // Force ESM mode so top-level await works

// ─── Memo cache ──────────────────────────────────────────────────────────────

interface CacheEntry<T> {
  value: T;
  createdAt: number;
  hits: number;
}

class MemoCache<T> {
  private _store = new Map<string, CacheEntry<T>>();
  private _totalCalls = 0;
  private _cacheHits = 0;

  get(key: string): T | undefined {
    this._totalCalls++;
    const entry = this._store.get(key);
    if (entry) {
      entry.hits++;
      this._cacheHits++;
      return entry.value;
    }
    return undefined;
  }

  set(key: string, value: T): void {
    this._store.set(key, { value, createdAt: Date.now(), hits: 0 });
  }

  get size() { return this._store.size; }

  stats() {
    const rate = this._totalCalls > 0
      ? (this._cacheHits / this._totalCalls * 100).toFixed(1)
      : "0.0";
    return { size: this._store.size, calls: this._totalCalls, hitRate: `${rate}%` };
  }
}

// ─── Expensive computation ────────────────────────────────────────────────────

function expensiveCompute(x: number): number {
  // Simulate work (O(n) sum)
  let acc = 0;
  for (let i = 0; i < x * 100; i++) acc += i;
  return acc;
}

// ─── Memoized wrapper ────────────────────────────────────────────────────────

const cache = new MemoCache<number>();

function memoCompute(x: number): number {
  // BUG: key includes Date.now() — every call is "unique" even for same x
  const key = JSON.stringify({ x, t: Date.now() });
  //                                ^^^^^^^^^^^^^ should NOT be here

  const cached = cache.get(key);
  if (cached !== undefined) return cached;

  const result = expensiveCompute(x);
  cache.set(key, result);
  return result;
}

// ─── Main ────────────────────────────────────────────────────────────────────

const ROUNDS = 15;
const INPUTS = [4, 7, 4, 4, 7, 12, 7, 4, 12, 4]; // repeated inputs — should hit cache

// Give the debugger UI a moment to connect before we pause
await new Promise(r => setTimeout(r, 1500));

console.log("Running memoized computation...\n");
debugger; // ← pause here: cache is empty, about to start rounds

for (let round = 0; round < ROUNDS; round++) {
  for (const x of INPUTS) {
    memoCompute(x);
  }
  const s = cache.stats();
  console.log(
    `Round ${String(round + 1).padStart(2)}: ` +
    `cache.size=${s.size.toString().padStart(3)}, ` +
    `hitRate=${s.hitRate}   ` +     // always 0% — cache never hits
    `← expected ~${Math.round(100 * (INPUTS.length - 3) / INPUTS.length)}%`
  );
  if (round === 2) debugger; // ← pause after round 3: cache.size should be 3 but is 30
}

// Pause here — inspect cache._store keys to see why they're all unique
console.log("\n=== Final stats ===");
console.log(cache.stats());
console.log("Expected cache size: 3 (unique x values: 4, 7, 12)");
console.log("Actual cache size:  ", cache.size, "(one entry per call — bug!)");
