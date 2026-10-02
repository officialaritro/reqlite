import struct, sys, zlib
raw = open(sys.argv[1], "rb").read()
head, data = raw.split(b"\n", 1)
w, h = map(int, head.split())
rows = b"".join(b"\0" + data[y * w * 4:(y + 1) * w * 4] for y in range(h))
def chunk(t, d):
    return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
png = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(rows, 9)) + chunk(b"IEND", b"")
open(sys.argv[2], "wb").write(png)
