import argparse
from dap_cli.session import DAPSession
from dap_cli.repl import DAPREPL

def main():
    parser = argparse.ArgumentParser(description="DAP CLI REPL")
    
    # We use nargs=argparse.REMAINDER so we can pass things like 'python3 -m debugpy.adapter' directly
    parser.add_argument("--command", nargs=argparse.REMAINDER, help="Command to run the debug adapter")
    parser.add_argument("--port", type=int, help="Port to attach to an existing debug adapter")
    parser.add_argument("--adapter", type=str, default="python", help="Adapter type (python, pwa-node, etc)")
    parser.add_argument("--pendingTargetId", type=str, help="Pending target ID for multi-session adapters like js-debug")
    parser.add_argument("--serve", action="store_true", help="Start the Real-Time Web Debugger UI Server")
    parser.add_argument("--open-browser", action="store_true", help="Automatically open the Web UI in the default browser")
    
    args, unknown = parser.parse_known_args()
    
    if not args.command and not args.port:
        parser.print_help()
        return

    # If --command was used but the first item in the remainder is just the program name, 
    # we might need to handle it. Actually, argparse is weird with REMAINDER if it's not at the end.
    command_list = args.command
    if command_list and command_list[0] == "--command":
        command_list = command_list[1:]

    try:
        session = DAPSession(command=command_list, port=args.port, adapter_type=args.adapter, pending_target_id=args.pendingTargetId)
        
        if args.serve:
            from dap_cli.web_server import start_server
            print("[INFO] Starting Web Server...")
            start_server(session, port=8000, open_browser=args.open_browser)
            
        repl = DAPREPL(session)
        repl.run()
    except Exception as e:
        print(f"Failed to start DAP CLI: {e}")

if __name__ == "__main__":
    main()
