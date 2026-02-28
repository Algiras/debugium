import json
import socket
import subprocess
import threading
import sys
import os
from typing import Dict, Any, Optional, Callable

class DAPClient:
    def __init__(self, command: list[str] = None, port: int = None):
        """
        Initialize the DAP Client. 
        Can connect via a spawned subprocess (command) or attaching to an existing port (port).
        """
        self.process = None
        self.socket = None
        self.reader = None
        self.writer = None
        self.sequence_number = 1
        self.pending_requests: Dict[int, Any] = {}
        self.event_handlers: Dict[str, list[Callable]] = {}
        self._listening = False
        self._listen_thread = None

        if command:
            self._start_process(command)
        elif port:
            self._connect_socket(port)
        else:
            raise ValueError("Must provide either a command to launch or a port to attach.")

    def _start_process(self, command: list[str]):
        # Start the debug adapter process
        self.process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=sys.stderr, # Forward stderr
        )
        self.writer = self.process.stdin
        self.reader = self.process.stdout
        self._start_listening()

    def _connect_socket(self, port: int):
        self.socket = socket.create_connection(('localhost', port))
        
        # Wrap socket in file-like objects for easier reading/writing
        self.reader = self.socket.makefile('rb')
        self.writer = self.socket.makefile('wb')
        self._start_listening()

    def send_request(self, command: str, arguments: dict = None, wait: bool = True) -> Any:
        """Sends a DAP request. If wait is True, blocks until a response is received."""
        req = {
            "seq": self.sequence_number,
            "type": "request",
            "command": command
        }
        if arguments:
            req["arguments"] = arguments

        self.sequence_number += 1
        seq = req["seq"]
        
        if not wait:
            self.pending_requests[seq] = {"event": None, "response": None}
            self._send_message(req)
            return seq

        # Use an event to block until the response is received
        event = threading.Event()
        self.pending_requests[seq] = {
            "event": event,
            "response": None
        }

        self._send_message(req)
        
        # Wait for the background thread to receive the response
        event.wait(timeout=10.0) # 10 second timeout
        
        result = self.pending_requests.pop(seq, None)
        if result is None or result["response"] is None:
            raise TimeoutError(f"Request {command} timed out")
            
        resp = result["response"]
        if not resp.get("success", False):
            raise Exception(f"DAP Error: {resp.get('message', 'Unknown error')} - {resp}")
            
        return resp

    def emit_event(self, event: str, body: dict = None):
        """Sends a custom event (usually done by server, but defined by protocol)"""
        pass # Client rarely sends events

    def register_event_handler(self, event_type: str, handler: Callable):
        if event_type not in self.event_handlers:
            self.event_handlers[event_type] = []
        self.event_handlers[event_type].append(handler)

    def _send_message(self, message: dict):
        body = json.dumps(message).encode('utf-8')
        header = f"Content-Length: {len(body)}\r\n\r\n".encode('ascii')
        
        self.writer.write(header)
        self.writer.write(body)
        self.writer.flush()

    def _start_listening(self):
        self._listening = True
        self._listen_thread = threading.Thread(target=self._listen_loop, daemon=True)
        self._listen_thread.start()

    def _listen_loop(self):
        while self._listening:
            try:
                # Read Headers
                content_length = 0
                while True:
                    line = self.reader.readline()
                    if not line:
                        self._listening = False
                        break
                    
                    line_str = line.decode('ascii').strip()
                    if line_str == "":
                        break # End of headers
                    if line_str.lower().startswith("content-length:"):
                        content_length = int(line_str.split(":")[1].strip())
                
                if not self._listening or content_length == 0:
                    continue
                
                # Read Body
                body_bytes = self.reader.read(content_length)
                message = json.loads(body_bytes.decode('utf-8'))
                self._handle_message(message)
                
            except Exception as e:
                # print(f"Error reading from DAP stream: {e}", file=sys.stderr)
                self._listening = False
                break

    def _handle_message(self, message: dict):
        print(f"[RAW IN] {message}", flush=True)
        
        if hasattr(self, 'raw_handlers'):
            for handler in self.raw_handlers:
                try:
                    handler(message)
                except Exception as e:
                    pass
                    
        msg_type = message.get("type")
        
        if msg_type == "response":
            req_seq = message.get("request_seq")
            if req_seq in self.pending_requests:
                req_data = self.pending_requests[req_seq]
                req_data["response"] = message
                if req_data.get("event"):
                    req_data["event"].set()
                
        elif msg_type == "request":
            command = message.get("command")
            if command == "startDebugging":
                args = message.get("arguments", {})
                config = args.get("configuration", {})
                pending_id = config.get("__pendingTargetId")
                
                self._send_message({
                    "seq": self.sequence_number,
                    "type": "response",
                    "request_seq": message.get("seq"),
                    "command": "startDebugging",
                    "success": True
                })
                self.sequence_number += 1
                
                if pending_id:
                    print(f"\n[INFO] Child session requested: {pending_id}", flush=True)
                else:
                    print(f"\n[INFO] Child session requested with no ID in config: {config}", flush=True)

        elif msg_type == "event":
            event_type = message.get("event")
            print(f"[RAW EVENT] {event_type}", flush=True)
            handlers = self.event_handlers.get(event_type, [])
            for handler in handlers:
                try:
                    handler(message)
                except Exception as e:
                    print(f"Error in event handler for {event_type}: {e}", file=sys.stderr, flush=True)

    def kill(self):
        self._listening = False
        if self.process:
            self.process.terminate()
            self.process.wait()
        if self.socket:
            self.socket.close()
