"""Minimal fake Home Assistant for end-to-end testing nowplayingd.

Serves:
  POST /api/states/<entity>   – logs the pushed state
  GET  /api/websocket         – auth handshake (token "tok"), then sends a few
                                call_service events and keeps the socket open

Requires: pip install aiohttp
"""
import asyncio
import json
import sys

from aiohttp import web

TOKEN = "tok"
ENTITY = "media_player.testbox"


async def states(req):
    print("POST", req.path, req.headers.get("Authorization"), (await req.text())[:500], flush=True)
    return web.json_response({}, status=201)


async def wsh(req):
    ws = web.WebSocketResponse()
    await ws.prepare(req)
    await ws.send_json({"type": "auth_required"})
    auth = json.loads((await ws.receive()).data)
    if auth.get("access_token") != TOKEN:
        await ws.send_json({"type": "auth_invalid", "message": "bad token"})
        return ws
    await ws.send_json({"type": "auth_ok"})
    print("SUB", (await ws.receive()).data, flush=True)

    await asyncio.sleep(1)
    events = [
        ("volume_set", {"entity_id": [ENTITY], "volume_level": 0.3}),
        ("media_next_track", {"entity_id": "media_player.other"}),  # must be ignored
        ("media_play_pause", {"entity_id": ENTITY}),
    ]
    for service, data in events:
        await ws.send_json({"type": "event", "event": {"data": {
            "domain": "media_player", "service": service, "service_data": data}}})
    await asyncio.sleep(10)
    return ws


app = web.Application()
app.add_routes([web.post("/api/states/{e}", states), web.get("/api/websocket", wsh)])
port = int(sys.argv[1]) if len(sys.argv) > 1 else 8999
web.run_app(app, host="127.0.0.1", port=port, print=None)
