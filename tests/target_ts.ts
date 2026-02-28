/**
 * Debugium TypeScript test target.
 * Run via:  debugium launch tests/target_ts.ts --adapter node --serve
 *           (ts-node must be available: npm i -g ts-node typescript)
 */

// ── Types ─────────────────────────────────────────────────────── //
interface DebugSession {
    id: string;
    adapter: "python" | "node" | "lldb" | "custom";
    paused: boolean;
    threadId?: number;
    stackDepth: number;
}

type Classified = "fizz" | "buzz" | "fizzbuzz" | `num:${number}`;

// ── Functions ─────────────────────────────────────────────────── //
function classify(n: number): Classified {
    if (n % 15 === 0) return "fizzbuzz";
    if (n % 3 === 0) return "fizz";
    if (n % 5 === 0) return "buzz";
    return `num:${n}`;
}

function* range(start: number, end: number, step = 1): Generator<number> {
    for (let i = start; i < end; i += step) yield i;
}

class SessionManager {
    private sessions = new Map<string, DebugSession>();

    add(session: DebugSession): void {
        this.sessions.set(session.id, session);
    }

    pause(id: string, threadId: number): boolean {
        const s = this.sessions.get(id);
        if (!s) return false;
        s.paused = true;
        s.threadId = threadId;
        return true;
    }

    get(id: string): DebugSession | undefined {
        return this.sessions.get(id);
    }

    summary(): Record<string, string> {
        const out: Record<string, string> = {};
        for (const [id, s] of this.sessions) {
            out[id] = `${s.adapter}:${s.paused ? "paused" : "running"}`;
        }
        return out;
    }
}

// ── Main ──────────────────────────────────────────────────────── //
async function main(): Promise<void> {
    // Breakpoint target 1: types + generator
    const nums = [...range(1, 16)];
    const labels: Classified[] = nums.map(classify);
    console.log("Labels:", labels);

    // Breakpoint target 2: class usage
    const mgr = new SessionManager();
    mgr.add({ id: "py-1", adapter: "python", paused: false, stackDepth: 0 });
    mgr.add({ id: "js-1", adapter: "node", paused: false, stackDepth: 0 });
    mgr.pause("py-1", 1);

    const session = mgr.get("py-1")!;
    console.log("Paused session:", session);

    // Breakpoint target 3: structured output
    const summary = mgr.summary();
    const metadata = {
        tool: "debugium",
        version: [0, 1, 0],
        sessions: summary,
        nums: nums.slice(0, 5),
    };
    console.log("Summary:", metadata);
}

main().catch(console.error);
