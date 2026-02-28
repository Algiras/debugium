import sys
import os
import argparse
from typing import Optional
from dap_cli.session import DAPSession

class DAPREPL:
    def __init__(self, session: DAPSession):
        self.session = session
        self._running = True

    def run(self):
        print("Starting DAP CLI REPL...")
        print("Type 'help' for available commands.")
        
        while self._running:
            try:
                cmd_str = input("(dap) ")
                if not cmd_str.strip():
                    continue
                
                parts = cmd_str.split(" ", 1)
                cmd = parts[0].lower()
                args = parts[1] if len(parts) > 1 else ""
                
                self._handle_command(cmd, args)
                
            except EOFError:
                break
            except KeyboardInterrupt:
                print("\nInterrupted.")
            except Exception as e:
                print(f"Error executing command: {e}")
                
        self.session.disconnect()

    def _handle_command(self, cmd: str, args: str):
        if cmd == "exit" or cmd == "quit":
            self._running = False
            
        elif cmd == "help":
            print("Commands:")
            print("  launch <program_path>  - Launch a program to debug")
            print("  attach <port>          - Attach to a debugger port")
            print("  init                   - Send initial configuration (call after launch)")
            print("  break <file>:<line>    - Set a breakpoint")
            print("  config_done            - Finish configuration and start running")
            print("  continue               - Continue execution")
            print("  step                   - Step into")
            print("  next                   - Step over")
            print("  eval <expression>      - Evaluate an expression")
            print("  stack                  - Print stack trace")
            print("  exit                   - Exit the REPL")
            
        elif cmd == "init":
            self.session.initialize()
            print("[INFO] Initialized DAP connection")
            
        elif cmd == "launch":
            if not args:
                print("Usage: launch <program_path>")
                return
            # Launch often needs to be followed by configurationDone or an init
            self.session.launch(program=args.strip())
            print(f"[INFO] Launched {args}")
            
        elif cmd == "attach":
            if not args:
                print("Usage: attach <port>")
                return
            self.session.attach(port=int(args.strip()))
            print(f"[INFO] Attached to port {args}")
            
        elif cmd == "config_done":
            # Just to be completely safe, wait a tiny bit to make sure adapter finishes its own internal setup
            import time
            time.sleep(0.5) 
            self.session.configuration_done()
            print("[INFO] Configuration Done")

        elif cmd == "break":
            if ":" not in args:
                print("Usage: break <file>:<line>")
                return
            file_path, line_str = args.split(":", 1)
            line = int(line_str.strip())
            
            abs_path = os.path.abspath(file_path.strip())
            res = self.session.client.send_request("setBreakpoints", {
                "source": {"path": abs_path},
                "breakpoints": [{"line": line}]
            })
            bps = res.get("body", {}).get("breakpoints", [])
            for bp in bps:
                if bp.get("verified"):
                    print(f"[INFO] Breakpoint verified at line {bp.get('line')}")
                else:
                    print(f"[WARNING] Breakpoint unverified: {bp.get('message', 'unknown reason')}")
            
        elif cmd in ["continue", "c"]:
            # Need a thread ID; assuming default 1 for now (which is bad, but simple)
            self.session.client.send_request("continue", {"threadId": 1})
            print("[INFO] Continuing...")
            
        elif cmd in ["step", "s"]:
            self.session.client.send_request("stepIn", {"threadId": 1})
            
        elif cmd in ["next", "n"]:
            self.session.client.send_request("next", {"threadId": 1})

        elif cmd == "threads":
            try:
                res = self.session.client.send_request("threads")
                threads = res.get("body", {}).get("threads", [])
                for t in threads:
                    print(f"[{t.get('id')}] {t.get('name')}")
                if not threads:
                    print("No threads active.")
            except Exception as e:
                print(f"[ERROR] {e}")

        elif cmd == "eval":
            if not args:
                print("Usage: eval <expression>")
                return
            
            # Fetch the top frame ID to evaluate in context
            try:
                stack_res = self.session.client.send_request("stackTrace", {"threadId": 1})
                frames = stack_res.get("body", {}).get("stackFrames", [])
                if not frames:
                    print("[ERROR] No stack frames available.")
                    return
                frame_id = frames[0]["id"]
            except Exception as e:
                print(f"[ERROR] Could not fetch stack trace: {e}")
                return

            try:
                res = self.session.client.send_request("evaluate", {
                    "expression": args.strip(),
                    "context": "repl",
                    "frameId": frame_id
                })
                body = res.get("body", {})
                print(f"{body.get('result', '')}")
            except Exception as e:
                print(f"{e}")

        elif cmd == "stack":
            res = self.session.client.send_request("stackTrace", {"threadId": 1})
            frames = res.get("body", {}).get("stackFrames", [])
            for frame in frames:
                print(f"[{frame.get('id')}] {frame.get('name')} at {frame.get('source', {}).get('path')}:{frame.get('line')}")
                
        else:
            print(f"Unknown command: {cmd}")
