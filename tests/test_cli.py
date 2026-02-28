import subprocess
import threading
import time
import os
import sys
import queue

class CLIWrapper:
    def __init__(self, target_path):
        self.cli = subprocess.Popen(
            [sys.executable, "-m", "dap_cli.cli", "--command", sys.executable, "-m", "debugpy.adapter"],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1 # Line buffered
        )
        self.output_queue = queue.Queue()
        self.running = True
        
        # Read stdout in a background thread to avoid blocking
        self.reader_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.reader_thread.start()
        
    def _read_stdout(self):
        while self.running:
            char = self.cli.stdout.read(1)
            if char:
                self.output_queue.put(char)
            else:
                break
                
    def send(self, cmd):
        print(f"-> {cmd}")
        self.cli.stdin.write(cmd + "\n")
        self.cli.stdin.flush()
        
    def expect(self, text, timeout=5.0):
        start = time.time()
        buffer = ""
        while time.time() - start < timeout:
            try:
                char = self.output_queue.get(timeout=0.1)
                print(char, end="", flush=True)
                buffer += char
                if text in buffer:
                    return buffer
            except queue.Empty:
                continue
        raise TimeoutError(f"Expected '{text}' but got: {buffer}")
        
    def close(self):
        self.running = False
        self.cli.terminate()
        try:
            self.cli.wait(timeout=2)
        except subprocess.TimeoutExpired:
            self.cli.kill()

def test_cli_debug_flow():
    # Kill any dangling listeners on 5678 from previous aborted tests
    os.system("lsof -ti:5678 | xargs kill -9 >/dev/null 2>&1")
    
    # Ensure test target exists
    target = os.path.abspath("test_target.py")
    if not os.path.exists(target):
        print("Creating dummy test_target.py")
        with open(target, "w") as f:
            f.write("import time\nimport debugpy\ndebugpy.listen(5678)\ndebugpy.wait_for_client()\nfor i in range(5):\n    val = i * 2\n    print(val)\nprint('Done')\n")
            
    print(f"Starting test targeting {target}")
    
    # Start target in background
    target_proc = subprocess.Popen([sys.executable, target])
    time.sleep(1) # wait for listen
    
    cli = CLIWrapper(target)
    try:
        # Wait for prompt to appear
        cli.expect("(dap)")
        
        print("\n--- Testing init ---")
        cli.send("init")
        cli.expect("[INFO] Initialized DAP connection")
        cli.expect("(dap)")
        
        print("\n--- Testing attach ---")
        cli.send("attach 5678")
        cli.expect("[INFO] Attached to port 5678")
        cli.expect("(dap)") 
        cli.expect("[EVENT] Initialized")
        
        print("\n--- Testing breakpoints ---")
        # Line 6 is 'val = i * 2'
        cli.send(f"break {target}:6")
        cli.expect("Breakpoint verified at line")
        cli.expect("(dap)")
        
        print("\n--- Testing configuration done ---")
        cli.send("config_done")
        cli.expect("[INFO] Configuration Done")
        cli.expect("(dap)")
        
        print("\n--- Testing continue to hit breakpoint ---")
        cli.send("continue")
        cli.expect("Stopped (breakpoint) on Thread 1")
        cli.expect("(dap)")
        
        print("\n--- Testing evaluation (i) ---")
        cli.send("eval i")
        output = cli.expect("(dap)")
        assert "0\n(dap)" in output, f"Expected 0, got {output}"
        
        print("\n--- Testing step ---")
        cli.send("next")
        cli.expect("Stopped (step) on Thread 1")
        cli.expect("(dap)")
        
        print("\n--- Testing evaluation (val) ---")
        cli.send("eval val")
        output = cli.expect("(dap)")
        assert "0\n(dap)" in output, f"Expected 0, got {output}"
        
        print("\n--- All tests passed! ---")
    finally:
        cli.close()
        target_proc.terminate()

if __name__ == "__main__":
    test_cli_debug_flow()
