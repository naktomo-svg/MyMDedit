"""Accepts TCP on 127.0.0.1:18440-18459 and logs port + first bytes of each connection."""
import socket, selectors, sys, time
sel = selectors.DefaultSelector()
out = open(sys.argv[1], "a", encoding="utf-8", buffering=1)
for port in range(18440, 18460):
    s = socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    s.bind(("127.0.0.1", port)); s.listen(); s.setblocking(False)
    sel.register(s, selectors.EVENT_READ, ("listen", port))
end = time.time() + float(sys.argv[2])
while time.time() < end:
    for key, _ in sel.select(timeout=0.5):
        kind, port = key.data
        if kind == "listen":
            c, _ = key.fileobj.accept(); c.setblocking(False)
            sel.register(c, selectors.EVENT_READ, ("conn", port))
            out.write(f"{time.strftime('%X')} CONNECT port={port}\n")
        else:
            try: data = key.fileobj.recv(4096)
            except OSError: data = b""
            if data:
                first = data.split(b"\r\n")[0][:120]
                out.write(f"{time.strftime('%X')} DATA port={port} tls={data[:1]==b'\x16'} first={first!r}\n")
            sel.unregister(key.fileobj); key.fileobj.close()
