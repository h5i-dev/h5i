#!/usr/bin/env python3
"""TCP clients to a framed UART (h5i_app_embedded::link), one at a time.

    uart-gateway.py LISTEN_PORT UART_PORT [CONNECTION_BOUND_SECONDS]

Listens on 127.0.0.1:LISTEN_PORT. For each client, in turn: sends
FLAG 'O' id to the UART, then the client's bytes escaped (so no client can
write a marker), then FLAG 'C' id when the client half-closes or the bound
expires. Of what the device sends, the client receives only the bytes
between the device's FLAG 'O' id and FLAG 'C' id for its own id: a response
the device wrote for an earlier connection is never forwarded to a later
one. Clients wait in the listen backlog while another is served; each holds
the line for at most the bound (the device enforces its own bound too).

UART_PORT is Renode's server socket terminal (thermostat.resc); it must be
reachable only by this gateway, since a raw writer to the line can forge
markers.
"""
import select
import socket
import sys
import time

FLAG, ESC, OPEN, CLOSE = 0x7E, 0x7D, ord("O"), ord("C")


def escape(data):
    out = bytearray()
    for b in data:
        if b in (FLAG, ESC):
            out += bytes([ESC, b ^ 0x20])
        else:
            out.append(b)
    return bytes(out)


class Decoder:
    """The device's output: data bytes tagged with the connection they belong to."""

    def __init__(self):
        self.state = "data"
        self.kind = None
        self.seg = 0      # the connection the device is talking to, 0 for none
        self.closed = set()

    def feed(self, data):
        """Yield (segment id, data byte) and record closed segments."""
        for b in data:
            if self.state == "esc":
                self.state = "data"
                yield self.seg, b ^ 0x20
            elif self.state == "flag":
                self.state = "marker" if b in (OPEN, CLOSE) else "data"
                self.kind = b
            elif self.state == "marker":
                self.state = "data"
                if self.kind == OPEN:
                    self.seg = b
                else:
                    self.closed.add(b)
                    if self.seg == b:
                        self.seg = 0
            elif b == FLAG:
                self.state = "flag"
            elif b == ESC:
                self.state = "esc"
            else:
                yield self.seg, b


def main():
    listen_port, uart_port = int(sys.argv[1]), int(sys.argv[2])
    bound = float(sys.argv[3]) if len(sys.argv) > 3 else 6.0
    for _ in range(240):
        try:
            uart = socket.create_connection(("127.0.0.1", uart_port))
            break
        except OSError:
            time.sleep(0.5)
    else:
        sys.exit("no UART socket on port %d" % uart_port)
    srv = socket.socket()
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    srv.bind(("127.0.0.1", listen_port))
    srv.listen(64)
    print("gateway: 127.0.0.1:%d -> uart:%d" % (listen_port, uart_port), flush=True)
    dec = Decoder()
    cid = 0
    while True:
        # Between clients, drop whatever the device still sends.
        while True:
            r, _, _ = select.select([srv, uart], [], [])
            if uart in r:
                for _ in dec.feed(uart.recv(4096)):
                    pass
            if srv in r:
                break
        client, _ = srv.accept()
        cid = cid % 255 + 1
        dec.closed.discard(cid)
        uart.sendall(bytes([FLAG, OPEN, cid]))
        deadline = time.time() + bound
        client_open = True
        while cid not in dec.closed:
            left = deadline - time.time()
            if left <= 0:
                break
            socks = [uart] + ([client] if client_open else [])
            r, _, _ = select.select(socks, [], [], left)
            if client_open and client in r:
                try:
                    data = client.recv(4096)
                except OSError:
                    data = b""
                if data:
                    uart.sendall(escape(data))
                else:
                    client_open = False
                    uart.sendall(bytes([FLAG, CLOSE, cid]))
            if uart in r:
                out = bytes(b for seg, b in dec.feed(uart.recv(4096)) if seg == cid)
                if out:
                    try:
                        client.sendall(out)
                    except OSError:
                        pass
        if client_open:
            uart.sendall(bytes([FLAG, CLOSE, cid]))
        try:
            client.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        client.close()


if __name__ == "__main__":
    main()
