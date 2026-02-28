import subprocess
import time
import os
import sys
from dap_cli.client import DAPClient

target = os.path.abspath("target.js")
port = 8124
server_path = os.path.abspath("js-debug/js-debug/src/dapDebugServer.js")

server_proc = subprocess.Popen(["node", server_path, str(port)])
time.sleep(1)

target_proc = subprocess.Popen(["node", "--inspect-brk=9229", target], stdout=subprocess.PIPE, stderr=subprocess.PIPE)
time.sleep(1)

client = DAPClient(port=port)
try:
    print("Init:", client.send_request("initialize", {
        "clientID": "dap-cli",
        "clientName": "DAP CLI REPL",
        "adapterID": "pwa-node",
        "pathFormat": "path"
    }))
    client.send_request("attach", {
        "type": "pwa-node",
        "request": "attach",
        "port": 9229
    }, wait=False)
    time.sleep(0.5)
    print("Asking for Threads...")
    client.send_request("threads", wait=False)
    time.sleep(0.5)
    print("Sending Config Done...")
    client.send_request("configurationDone", wait=False)
    time.sleep(2)
    print("Asking for Threads again...")
    client.send_request("threads", wait=False)
    time.sleep(2)
finally:
    client.socket.close()
    server_proc.terminate()
    target_proc.terminate()
