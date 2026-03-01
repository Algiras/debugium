"""Simple LRU-like cache with a subtle bug."""

from collections import OrderedDict


class Cache:
    def __init__(self, capacity: int = 4):
        self.capacity = capacity
        self._store: OrderedDict = OrderedDict()
        self._hits = 0
        self._misses = 0

    def get(self, key: str):
        if key in self._store:
            self._store.move_to_end(key)
            self._hits += 1
            return self._store[key]
        self._misses += 1
        return None

    def put(self, key: str, value):
        if key in self._store:
            self._store.move_to_end(key)
        self._store[key] = value
        # BUG: evicts NEWEST instead of OLDEST (last=True removes from end = most recent)
        if len(self._store) > self.capacity:
            self._store.popitem(last=True)

    def stats(self) -> dict:
        total = self._hits + self._misses
        rate = self._hits / total if total else 0.0
        return {"hits": self._hits, "misses": self._misses, "hit_rate": rate}

    def __repr__(self):
        return f"Cache(capacity={self.capacity}, entries={list(self._store.keys())})"
