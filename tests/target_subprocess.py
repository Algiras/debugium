"""Parent script: spawns a child Python process via subprocess.
Used to test debugpy's subProcess auto-attach feature.
"""
import subprocess
import sys
import os

def main():
    child_script = os.path.join(os.path.dirname(os.path.abspath(__file__)), "target_subprocess_child.py")
    result = subprocess.run(
        [sys.executable, child_script],
        capture_output=True,
        text=True,
    )
    output = result.stdout.strip()
    print(f"parent: child returned: {output}")
    return output

if __name__ == "__main__":
    main()
