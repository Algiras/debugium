import asyncio
import os
import threading
import json
import webbrowser
from typing import List
from fastapi import FastAPI, WebSocket, WebSocketDisconnect
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse
import uvicorn

app = FastAPI()

class ConnectionManager:
    def __init__(self):
        self.active_connections: List[WebSocket] = []
        self.loop = None

    async def connect(self, websocket: WebSocket):
        await websocket.accept()
        self.active_connections.append(websocket)

    def disconnect(self, websocket: WebSocket):
        self.active_connections.remove(websocket)

    async def broadcast(self, message: str):
        for connection in self.active_connections:
            await connection.send_text(message)
            
    def broadcast_sync(self, message: str):
        if self.loop and self.loop.is_running():
            asyncio.run_coroutine_threadsafe(self.broadcast(message), self.loop)

manager = ConnectionManager()
active_sessions = {}

STATIC_DIR = os.path.join(os.path.dirname(__file__), "static")
app.mount("/static", StaticFiles(directory=STATIC_DIR), name="static")

@app.on_event("startup")
async def startup_event():
    manager.loop = asyncio.get_running_loop()

@app.get("/")
async def get():
    return FileResponse(os.path.join(STATIC_DIR, "index.html"))

@app.get("/source")
async def get_source(path: str):
    try:
        if not os.path.isabs(path):
            path = os.path.abspath(path)
        if not os.path.exists(path):
            return JSONResponse({"error": "File not found"}, status_code=404)
        with open(path, "r", encoding="utf-8") as f:
            lines = f.readlines()
        return JSONResponse({"lines": lines})
    except Exception as e:
        return JSONResponse({"error": str(e)}, status_code=500)

@app.websocket("/ws")
async def websocket_endpoint(websocket: WebSocket):
    await manager.connect(websocket)
    try:
        while True:
            data = await websocket.receive_text()
            try:
                payload = json.loads(data)
                cmd = payload.get("cmd")
                args = payload.get("args", {})
                target_session_id = payload.get("session_id", "Root")
                
                target_session = active_sessions.get(target_session_id)
                
                if target_session and cmd == "request":
                    req_cmd = args.get("command")
                    req_args = args.get("arguments", {})
                    target_session.client.send_request(req_cmd, req_args, wait=False)
            except Exception as e:
                print(f"Error handling WS message: {e}")
                    
    except WebSocketDisconnect:
        manager.disconnect(websocket)

def start_server(session, port=8000, open_browser=False):
    global active_sessions
    
    if not hasattr(session, 'id'):
        session.id = "Root"
    
    active_sessions[session.id] = session
    
    # Hook the client's handle_message to broadcast to our manager
    def raw_message_handler(message):
        try:
            msg_str = json.dumps({
                "session_id": session.id,
                "msg": message
            })
            manager.broadcast_sync(msg_str)
        except Exception as e:
            pass
            
    if not hasattr(session.client, 'raw_handlers'):
        session.client.raw_handlers = []
    session.client.raw_handlers.append(raw_message_handler)

    def run():
        uvicorn.run(app, host="127.0.0.1", port=port, log_level="error")
        
    t = threading.Thread(target=run, daemon=True)
    t.start()
    
    if open_browser:
        # Give server a second to start, then open browser
        import time
        time.sleep(1)
        webbrowser.open(f"http://127.0.0.1:{port}")
