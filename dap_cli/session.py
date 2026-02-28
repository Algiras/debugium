import os
from dap_cli.client import DAPClient

class DAPSession:
    def __init__(self, command: list[str] = None, port: int = None, adapter_type: str = "python", pending_target_id: str = None):
        self.client = DAPClient(command=command, port=port)
        self.adapter_type = adapter_type
        self.pending_target_id = pending_target_id
        self.id = pending_target_id or "Root"
        self.capabilities = {}
        self.is_initialized = False
        
        # Setup basic event handlers
        self.client.register_event_handler("initialized", self._on_initialized)
        self.client.register_event_handler("stopped", self._on_stopped)
        self.client.register_event_handler("terminated", self._on_terminated)
        self.client.register_event_handler("output", self._on_output)

    def initialize(self):
        """Sends the initial initialize request"""
        resp = self.client.send_request("initialize", {
            "clientID": "dap-cli",
            "clientName": "DAP CLI REPL",
            "adapterID": self.adapter_type, 
            "pathFormat": "path",
            "linesStartAt1": True,
            "columnsStartAt1": True,
            "supportsVariableType": True,
            "supportsVariablePaging": False,
            "supportsRunInTerminalRequest": False
        })
        self.capabilities = resp.get("body", {})

    def launch(self, program: str, args: list[str] = None, cwd: str = None):
        """Launch a program"""
        launch_args = {
            "type": self.adapter_type,
            "request": "launch",
            "program": os.path.abspath(program),
            "console": "internalConsole",
            "cwd": cwd or os.getcwd()
        }
        if self.adapter_type == "python":
            launch_args["debugOptions"] = ["RedirectOutput", "ShowReturnValue"]
            launch_args["justMyCode"] = False
        else:
            launch_args["stopOnEntry"] = True
            
        if self.pending_target_id:
            launch_args["__pendingTargetId"] = self.pending_target_id
            
        if args:
            launch_args["args"] = args
        if cwd:
            launch_args["cwd"] = cwd
            
        print(f"[DEBUG] Sending launch request: {launch_args}")
        try:
            res = self.client.send_request("launch", launch_args, wait=False)
            print(f"[DEBUG] Launch request sent (seq {res})")
        except Exception as e:
            print(f"[ERROR] Launch failed: {e}")

    def attach(self, port: int, host: str = "127.0.0.1"):
        """Attach to a running program"""
        attach_args = {
            "type": self.adapter_type,
            "request": "attach",
            "port": port,
            "host": host,
            "trace": True
        }
        if self.pending_target_id:
            attach_args["__pendingTargetId"] = self.pending_target_id
            
        self.client.send_request("attach", attach_args, wait=False)

    def configuration_done(self):
        """Signal that configuration (breakpoints etc) is done"""
        self.client.send_request("configurationDone")

    def disconnect(self):
        self.client.kill()

    # --- Event Handlers ---
    def _on_initialized(self, event):
        print("\n[EVENT] Initialized received from adapter. Configuration can now proceed.")
        self.is_initialized = True

    def _on_stopped(self, event):
        body = event.get("body", {})
        reason = body.get("reason", "unknown")
        thread_id = body.get("threadId", "unknown")
        print(f"\n[EVENT] Stopped ({reason}) on Thread {thread_id}")

    def _on_terminated(self, event):
        print("\n[EVENT] Debuggee terminated")

    def _on_output(self, event):
        body = event.get("body", {})
        output = body.get("output", "")
        # Filter noisy telemetry
        if body.get("category") != "telemetry":
            print(f"[OUTPUT] {output}", end="")
