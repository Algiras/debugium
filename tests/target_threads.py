import threading

results = {}

def worker(name: str, n: int):
    total = sum(range(n))   # breakpoint here (line 6)
    results[name] = total

t1 = threading.Thread(target=worker, args=("alpha", 100), daemon=True)
t2 = threading.Thread(target=worker, args=("beta",  200), daemon=True)
t1.start()
t2.start()
t1.join()
t2.join()
print(results)
