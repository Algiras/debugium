#!/bin/bash
source .venv/bin/activate
python3 test_target.py &
sleep 1
python3 -m dap_cli.cli --port 5678
