"""Demo script for screenshots — runs slowly to allow UI to connect."""
import time
from cache import Cache

def demo():
    cache = Cache(capacity=3)

    keys = ["apple", "banana", "cherry"]
    for i, key in enumerate(keys):
        cache.put(key, i * 10)
        time.sleep(0.05)

    # access "apple" so it should be MRU
    result = cache.get("apple")

    # add a 4th item — bug: evicts newest instead of oldest
    cache.put("date", 99)
    time.sleep(0.05)

    present = {k: cache.get(k) for k in [*keys, "date"]}
    hit_rate = cache.stats()["hit_rate"]

    print(f"Cache contents: {present}")
    print(f"Hit rate: {hit_rate:.0%}")
    print(f"Expected: apple=0, banana=None (evicted), cherry=20, date=99")
    print(f"Bug: banana is still present, date is None (just evicted!)")

    return present

if __name__ == "__main__":
    print("=== Cache Bug Demo ===")
    demo()
    print("Done.")
