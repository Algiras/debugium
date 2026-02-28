import subprocess
import threading
import time
import os
import sys

target = os.path.abspath("target.js")
port = 8124
server_path = os.path.abspath("js-debug/js-debug/src/dapDebugServer.js")

server_proc = subprocess.Popen(["node", server_path, str(port)])
time.sleep(1)

target_proc = subprocess.Popen(["node", "--inspect-brk=9229", target], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
time.sleep(1)

cli = subprocess.Popen(
    [sys.executable, "-m", "dap_cli.cli", "--port", str(port), "--adapter", "pwa-node"],
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    stderr=subprocess.PIPE,
    text=True,
    bufsize=1
)

def read_output():
    for line in iter(cli.stdout.readline, ""):
        print(line, end="")

t = threading.Thread(target=read_output, daemon=True)
t.start()

def command(cmd):
    print(f"\n--- Sending: {cmd} ---")
    cli.stdin.write(cmd + "\n")
    cli.stdin.flush()
    time.sleep(1.5)

try:
    command("init")
    command("attach 9229")
    command(f"break {target}:3")
    command("config_done")
    # Send custom raw request "threads":
    # Wait, DAP CLI REPL doesn't have a built-in `threads` command!
    # Let me just run a short script that directly uses session.client instead of REPL.
    pass
    time.sleep(2)
finally:
    cli.terminate()
    server_proc.terminate()
    target_proc.terminate()
