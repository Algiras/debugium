"""
Multi-file debugging target.

Demonstrates two separate bugs across files:
  1. cache.py: LRU eviction removes newest instead of oldest
  2. transform.py: normalize() uses mutable default arg — history bleeds across calls

Run this and observe the wrong output. Use the debugger to find why.
"""

from cache import Cache
from scheduler import Scheduler, Job
from transform import normalize, running_average, threshold_filter


# ── Session A: Cache bug ─────────────────────────────────────────────── #

def run_cache_demo():
    cache = Cache(capacity=3)

    # populate
    for key, val in [("a", 1), ("b", 2), ("c", 3)]:
        cache.put(key, val)

    # access "a" so it becomes most-recently-used
    _ = cache.get("a")

    # add a 4th item — should evict "b" (oldest), but BUG evicts newest
    cache.put("d", 4)

    present = {k: cache.get(k) for k in ["a", "b", "c", "d"]}
    # expected: a=1, b=None (evicted), c=3, d=4
    # actual:   a=1, b=2,   c=3, d=None  (d was just evicted!)
    assert present["b"] is None, f"Expected b evicted, got {present}"
    return present


# ── Session B: Transform bug ─────────────────────────────────────────── #

def run_transform_demo():
    batch1 = [10.0, 20.0, 30.0]
    batch2 = [1.0, 2.0, 3.0]

    norm1 = normalize(batch1)
    # norm1 should be [0.0, 0.5, 1.0]

    norm2 = normalize(batch2)
    # BUG: norm2 should be [0.0, 0.5, 1.0] if independent,
    # but history from batch1 pollutes the range → values near 0.0

    avg = running_average(norm2)
    filtered = threshold_filter(avg, cutoff=0.3)

    return {"norm1": norm1, "norm2": norm2, "avg": avg, "filtered": filtered}


# ── Session C: Scheduler (works correctly — baseline) ────────────────── #

def run_scheduler_demo():
    sched = Scheduler()
    sched.submit(Job("low",  priority=1, fn=lambda: "low-result"))
    sched.submit(Job("high", priority=9, fn=lambda: "high-result"))
    sched.submit(Job("med",  priority=5, fn=lambda: "med-result"))

    completed = sched.run_all()
    order = [j.name for j in completed]
    # expected: ["high", "med", "low"]
    return {"order": order, "results": sched.results()}


if __name__ == "__main__":
    print("=== Cache demo ===")
    try:
        r = run_cache_demo()
        print("cache present:", r)
    except AssertionError as e:
        print("ASSERTION FAILED:", e)

    print("\n=== Transform demo ===")
    r = run_transform_demo()
    for k, v in r.items():
        print(f"  {k}: {v}")

    print("\n=== Scheduler demo ===")
    r = run_scheduler_demo()
    print("  order:", r["order"])
    print("  results:", r["results"])
