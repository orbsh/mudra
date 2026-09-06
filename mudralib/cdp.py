"""Minimal WebSocket (RFC6455, client-side) + CDP helpers.

Stdlib only. Just enough for CDP communication: text frames, continuation,
ping/pong, close.
"""

from __future__ import annotations

import base64
import hashlib
import json
import os
import socket
import struct
import urllib.parse
import urllib.request


class WsError(Exception):
    pass


class WsClient:
    """Minimal RFC6455 client (only client->server masking, text/binary frames)."""

    def __init__(self, url: str, timeout: float = 10.0):
        self.sock = self._connect(url, timeout)
        self._buf = b""

    def _connect(self, url: str, timeout: float) -> socket.socket:
        p = urllib.parse.urlparse(url)
        host, port = p.hostname, p.port or (443 if p.scheme == "wss" else 80)
        s = socket.create_connection((host, port), timeout=timeout)
        key = base64.b64encode(os.urandom(16)).decode()
        path = p.path or "/"
        if p.query:
            path += "?" + p.query
        req = (
            f"GET {path} HTTP/1.1\r\n"
            f"Host: {host}:{port}\r\n"
            "Upgrade: websocket\r\n"
            "Connection: Upgrade\r\n"
            f"Sec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        s.sendall(req.encode())
        resp = b""
        while b"\r\n\r\n" not in resp:
            chunk = s.recv(4096)
            if not chunk:
                raise WsError("handshake: eof before headers")
            resp += chunk
        head, _, self._buf = resp.partition(b"\r\n\r\n")
        if b" 101 " not in head.split(b"\r\n", 1)[0]:
            raise WsError(f"handshake failed: {head.splitlines()[0]!r}")
        return s

    def _recv_exact(self, n: int) -> bytes:
        while len(self._buf) < n:
            d = self.sock.recv(4096)
            if not d:
                raise WsError("connection closed")
            self._buf += d
        out, self._buf = self._buf[:n], self._buf[n:]
        return out

    def recv_frame(self) -> tuple:
        b0, b1 = self._recv_exact(2)
        fin, opcode = (b0 >> 7) & 1, b0 & 0x0F
        masked, ln = (b1 >> 7) & 1, b1 & 0x7F
        if ln == 126:
            ln = struct.unpack(">H", self._recv_exact(2))[0]
        elif ln == 127:
            ln = struct.unpack(">Q", self._recv_exact(8))[0]
        mask = self._recv_exact(4) if masked else None
        payload = self._recv_exact(ln)
        if mask:
            payload = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        return fin, opcode, payload

    def send_frame(self, opcode: int, payload: bytes = b"") -> None:
        mask = os.urandom(4)
        ln = len(payload)
        first = 0x80 | opcode
        if ln < 126:
            frame = bytes([first, 0x80 | ln])
        elif ln < 65536:
            frame = bytes([first, 0x80 | 126]) + struct.pack(">H", ln)
        else:
            frame = bytes([first, 0x80 | 127]) + struct.pack(">Q", ln)
        frame += mask
        frame += bytes(x ^ mask[i % 4] for i, x in enumerate(payload))
        self.sock.sendall(frame)

    def send_text(self, s: str) -> None:
        self.send_frame(0x1, s.encode())

    def recv_text(self, timeout: float | None = None) -> str:
        """Read one full text message; replies to pings and handles continuation/close automatically."""
        self.sock.settimeout(timeout)
        out = bytearray()
        while True:
            fin, opcode, payload = self.recv_frame()
            if opcode == 0x9:            # ping -> pong
                self.send_frame(0xA, payload)
                continue
            if opcode == 0x8:            # close
                raise WsError("peer closed connection")
            if opcode in (0x1, 0x0):     # text / continuation
                out += payload
                if fin:
                    return out.decode()
            # ignore other opcodes

    def close(self) -> None:
        try:
            self.send_frame(0x8)
        except OSError:
            pass
        try:
            self.sock.close()
        except OSError:
            pass


def get_browser_ws(port: int, timeout: float = 5.0) -> str:
    """Get the browser-level CDP websocket address from /json/version."""
    with urllib.request.urlopen(f"http://127.0.0.1:{port}/json/version", timeout=timeout) as r:
        return json.loads(r.read())["webSocketDebuggerUrl"]


def call(ws: WsClient, method: str, params: dict | None = None, _id: int = 0):
    """Send a CDP command and block until the response with the same id returns."""
    cmd = {"id": _id, "method": method, "params": params or {}}
    ws.send_text(json.dumps(cmd))
    while True:
        msg = json.loads(ws.recv_text())
        if msg.get("id") == _id:
            return msg
        # other messages (events / other responses) are dropped here; the event
        # stream is taken over by mudrad itself