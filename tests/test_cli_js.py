import subprocess
import threading
import time
import os
import sys
import queue

class CLIWrapper:
    def __init__(self, port, pending_id=None):
        cmd = [sys.executable, "-m", "dap_cli.cli", "--port", str(port), "--adapter", "pwa-node"]
        if pending_id:
            cmd.extend(["--pendingTargetId", pending_id])
            
        self.cli = subprocess.Popen(
            cmd,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1
        )
        self.output_queue = queue.Queue()
        self.running = True
        
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

def test_js_debug_flow():
    # Target script
    target = os.path.abspath("target.js")
    with open(target, "w") as f:
        f.write("let val = 0;\nfor (let i = 0; i < 5; i++) {\n  val = i * 2;\n  console.log(val);\n}\nconsole.log('Done');\n")

    port = 8124
    server_path = os.path.abspath("js-debug/js-debug/src/dapDebugServer.js")
    
    # Start dapDebugServer
    server_proc = subprocess.Popen(["node", server_path, str(port)])
    time.sleep(1) # wait for server to listen

    # Start target in background stopped
    target_proc = subprocess.Popen(["node", "--inspect-brk=9229", target])
    time.sleep(1)

    cli = CLIWrapper(port)
    try:
        cli.expect("(dap)")
        
        print("\n--- Testing init ---")
        cli.send("init")
        cli.expect("[INFO] Initialized DAP connection")
        cli.expect("(dap)")
        
        print("\n--- Testing JS Attach to trigger Child Session ---")
        cli.send("attach 9229")
        cli.expect("(dap)")
        
        cli.send("threads")
        cli.expect("(dap)")
        
        print("\n--- Sending root config_done to trigger startDebugging ---")
        cli.send("config_done")
        
        cli.send("threads")
        out = cli.expect("Child session requested:", timeout=10.0)
        
        # Extract pendingTargetId
        import re
        match = re.search(r"Child session requested: ([a-zA-Z0-9]+)", out)
        if not match:
            raise ValueError("Could not find pendingTargetId")
        pending_id = match.group(1)
        print(f"Got pending ID: {pending_id}")
        
        # Start child session!
        child_cli = CLIWrapper(port, pending_id=pending_id)
        try:
            child_cli.expect("(dap)")
            child_cli.send("init")
            child_cli.expect("[INFO] Initialized DAP connection")
            child_cli.expect("(dap)")
            
            # For the child session, we also send launch but with the pending ID, wait, or attach?
            # js-debug expects an 'attach' request for the child session? YES, `arguments.request` was 'attach' in `startDebugging`.
            # Let's send attach to the child!
            print("\n--- Testing Child JS Attach ---")
            child_cli.send(f"attach 9229")
            child_cli.expect("(dap)")
            
            print("\n--- Testing Breakpoints ---")
            child_cli.send(f"break {target}:3")
            child_cli.expect("(dap)")
    
            print("\n--- Testing Configuration Done ---")
            child_cli.send("config_done")
            child_cli.expect("Configuration Done")
            child_cli.expect("(dap)")
            
            print("\n--- Waiting for Breakpoint Hit ---")
            child_cli.expect("Stopped (breakpoint) on Thread")
            child_cli.expect("(dap)")
    
            print("\n--- Testing Eval ---")
            child_cli.send("eval i")
            out = child_cli.expect("(dap)")
            assert "0\n(dap)" in out, f"Expected 0, got {out}"
    
            print("\n--- All tests passed! ---")
        finally:
            child_cli.close()
    finally:
        cli.close()
        server_proc.terminate()
        target_proc.terminate()

if __name__ == "__main__":
    test_js_debug_flow()
