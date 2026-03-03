"""
Debugium multi-thread test target — two threads computing sums concurrently.
Run via: debugium launch tests/target_threads.py --adapter python --breakpoint $(pwd)/tests/target_threads.py:8
"""
import threading

results = {}

def worker(name: str, n: int):
    total = sum(range(n))   # breakpoint here (line 10)
    results[name] = total

t1 = threading.Thread(target=worker, args=("alpha", 100), daemon=True)
t2 = threading.Thread(target=worker, args=("beta",  200), daemon=True)
t1.start()
t2.start()
t1.join()
t2.join()
print(results)
