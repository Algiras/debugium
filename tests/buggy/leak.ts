/**
 * Memory leak demo — TypeScript
 *
 * A simple event-driven task queue where callbacks are registered per-job.
 * BUG 1: EventBus never removes listeners after a job completes → unbounded growth
 * BUG 2: ResultStore accumulates every intermediate result, never evicts old entries
 *
 * Hard to see by reading: looks like normal cleanup code.
 * Instantly obvious in the debugger: watch `bus._listeners` grow each iteration.
 */

// ─── EventBus ────────────────────────────────────────────────────────────────

type Listener = (payload: unknown) => void;

class EventBus {
  _listeners: Map<string, Listener[]> = new Map();

  on(event: string, fn: Listener): void {
    if (!this._listeners.has(event)) {
      this._listeners.set(event, []);
    }
    this._listeners.get(event)!.push(fn);
  }

  emit(event: string, payload: unknown): void {
    this._listeners.get(event)?.forEach(fn => fn(payload));
  }

  // BUG: off() exists but is never called after job completion
  off(event: string, fn: Listener): void {
    const fns = this._listeners.get(event);
    if (!fns) return;
    const idx = fns.indexOf(fn);
    if (idx !== -1) fns.splice(idx, 1);
  }
}

// ─── ResultStore ─────────────────────────────────────────────────────────────

class ResultStore {
  _results: Array<{ jobId: number; value: number; ts: number }> = [];

  save(jobId: number, value: number): void {
    // BUG: never evicts — just keeps appending
    this._results.push({ jobId, value, ts: Date.now() });
  }

  latest(jobId: number): number | undefined {
    // scan from end
    for (let i = this._results.length - 1; i >= 0; i--) {
      if (this._results[i].jobId === jobId) return this._results[i].value;
    }
    return undefined;
  }

  size(): number { return this._results.length; }
}

// ─── Job runner ──────────────────────────────────────────────────────────────

const bus = new EventBus();
const store = new ResultStore();

function processJob(jobId: number): void {
  const resultEvent = `job:${jobId}:result`;

  // Register a listener for this job's result
  const handler: Listener = (val) => {
    store.save(jobId, val as number);
  };
  bus.on(resultEvent, handler);
  // BUG: bus.off(resultEvent, handler) is never called — handler leaks

  // Simulate work
  const result = jobId * jobId + Math.floor(Math.random() * 10);
  bus.emit(resultEvent, result);
}

// ─── Main loop ───────────────────────────────────────────────────────────────

const JOBS = 20;

console.log("Starting job queue...");

for (let i = 1; i <= JOBS; i++) {
  processJob(i);

  const listenerCount = [...bus._listeners.values()].reduce((s, a) => s + a.length, 0);
  console.log(
    `Job ${String(i).padStart(2)}: ` +
    `result=${store.latest(i)}, ` +
    `listeners=${listenerCount}, ` +   // grows: 1, 2, 3, 4, ... never drops
    `storedResults=${store.size()}`
  );
}

// Breakpoint here — inspect bus._listeners and store._results
console.log("\n=== Final state ===");
console.log(`Total listeners leaked: ${[...bus._listeners.values()].reduce((s, a) => s + a.length, 0)}`);
console.log(`Total stored results:   ${store.size()}`);
console.log(`(Expected listeners: 0, expected results: ${JOBS})`);
