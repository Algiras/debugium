"""Child script spawned by target_subprocess.py.
Uses breakpoint() so debugpy will pause here after auto-attach.
"""

def compute(x: int) -> int:
    squared = x * x   # line 6
    breakpoint()       # line 7 — pauses for attached debugger
    return squared

value = compute(7)
print(f"child result: {value}")
