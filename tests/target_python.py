"""
Debugium Python test target — demonstrates variable inspection, loops, and exceptions.
Run via: debugium launch tests/target_python.py --adapter python --serve
"""

def fibonacci(n: int) -> list[int]:
    """Generate first n Fibonacci numbers."""
    seq = [0, 1]
    for _ in range(n - 2):
        seq.append(seq[-1] + seq[-2])
    return seq[:n]


def classify(value: int) -> str:
    """Classify a number."""
    if value % 15 == 0:
        return "fizzbuzz"
    elif value % 3 == 0:
        return "fizz"
    elif value % 5 == 0:
        return "buzz"
    else:
        return str(value)


class Counter:
    """A simple stateful counter."""
    def __init__(self, start: int = 0):
        self.value = start
        self.history: list[int] = []

    def increment(self, by: int = 1) -> int:
        self.value += by
        self.history.append(self.value)
        return self.value

    def __repr__(self) -> str:
        return f"Counter(value={self.value}, history={self.history})"


def main() -> None:
    # ── Breakpoint target 1 ──────────────────────────── #
    fibs = fibonacci(10)
    result = [classify(n) for n in fibs]
    print("Fibonacci classified:", result)

    # ── Breakpoint target 2 ──────────────────────────── #
    counter = Counter(start=10)
    for step in [1, 2, 3, 5, 8, 13]:
        counter.increment(step)
        label = classify(counter.value)
        print(f"  step={step} → counter={counter.value} ({label})")

    # ── Breakpoint target 3 ──────────────────────────── #
    data = {
        "name": "debugium",
        "version": (0, 1, 0),
        "adapters": ["python", "node", "lldb"],
        "active": True,
    }
    print("Final counter state:", counter)
    print("Metadata:", data)


if __name__ == "__main__":
    main()
