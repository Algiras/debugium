use std::collections::HashMap;
use std::time::SystemTime;

/// Compute a "heavy" value for x (simulated with a trivial formula).
fn compute(x: u64) -> u64 {
    x * x + x + 41
}

/// Look up or compute the result for `x`, storing it in `cache`.
/// BUG: the cache key includes the current nanosecond timestamp, so the cache
/// never actually hits — every call recomputes and stores a new entry.
fn memoized_compute(cache: &mut HashMap<String, u64>, x: u64) -> (u64, bool) {
    // ← BUG is on this line: key should be format!("{x}") not format!("{x}_{}")
    let key = format!(
        "{x}_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );

    if let Some(&cached) = cache.get(&key) {
        return (cached, true); // cache hit
    }

    let result = compute(x);
    cache.insert(key, result);
    (result, false) // cache miss
}

fn main() {
    let mut cache: HashMap<String, u64> = HashMap::new();
    let inputs = [1u64, 2, 3, 1, 2, 3, 1, 2, 3, 1];
    let rounds = 15;

    let mut total_hits = 0usize;
    let mut total_calls = 0usize;

    for round in 0..rounds {
        for &x in &inputs {
            let (result, hit) = memoized_compute(&mut cache, x);
            total_calls += 1;
            if hit {
                total_hits += 1;
            }
            if round == 0 {
                println!("  compute({x}) = {result}  [cache {}]", if hit { "HIT" } else { "MISS" });
            }
        }
    }

    let hit_rate = (total_hits as f64 / total_calls as f64) * 100.0;
    println!("\n--- Summary ---");
    println!("Total calls  : {total_calls}");
    println!("Cache hits   : {total_hits}  ({hit_rate:.1}%)");
    println!("Cache entries: {}  (should be 3, got {})", cache.len(), cache.len());

    if cache.len() > inputs.len() {
        println!("\n[BUG] Cache has {} entries for only {} unique inputs!", cache.len(), inputs.len());
        println!("      Fix: change the key to format!(\"{{x}}\") to drop the timestamp.");
    }
}
