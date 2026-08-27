"""Put the artwork on Apple's icon grid.

A macOS app icon is not the square it appears to be. The canvas is padding
plus a rounded square that fills 80.5% of it — 824 inside 1024 — and the rest
is transparent. An icon drawn edge to edge is therefore a size larger than
every neighbour in the Dock, which is what this one was: measured at 100%
against 83.2% for Zoom, 83.6% for ChatGPT and 80.5% for Axure, with
Battle.net and Anaconda sharing the same mistake.

Kept at 512 rather than the 1024 the grid is stated in, because the bundler
generates the @2x entry from whatever it is given and has no icon type for
the 2048 a 1024 source asks it to make.

    python3 scripts/icon-grid.py in.png out.png
"""
import zlib, struct, sys

def load(path):
    raw = open(path, "rb").read()
    pos, idat = 8, b""
    while pos < len(raw):
        ln = struct.unpack(">I", raw[pos:pos + 4])[0]
        typ = raw[pos + 4:pos + 8]
        d = raw[pos + 8:pos + 8 + ln]
        if typ == b"IHDR":
            w, h, depth, color = struct.unpack(">IIBB", d[:10])
            assert (depth, color) == (8, 6), (depth, color)
        if typ == b"IDAT":
            idat += d
        pos += 12 + ln
    buf = zlib.decompress(idat)
    stride, prev, rows, i = w * 4, bytearray(w * 4), [], 0
    for _ in range(h):
        f = buf[i]; i += 1
        line = bytearray(buf[i:i + stride]); i += stride
        for x in range(stride):
            a = line[x - 4] if x >= 4 else 0
            b = prev[x]
            c = prev[x - 4] if x >= 4 else 0
            if f == 1: line[x] = (line[x] + a) & 255
            elif f == 2: line[x] = (line[x] + b) & 255
            elif f == 3: line[x] = (line[x] + ((a + b) >> 1)) & 255
            elif f == 4:
                p = a + b - c
                pa, pb, pc = abs(p - a), abs(p - b), abs(p - c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                line[x] = (line[x] + pr) & 255
        rows.append(bytes(line)); prev = line
    return w, h, rows

def save(path, w, h, rows):
    def chunk(typ, data):
        c = struct.pack(">I", len(data)) + typ + data
        return c + struct.pack(">I", zlib.crc32(typ + data) & 0xFFFFFFFF)
    body = b"".join(b"\x00" + bytes(r) for r in rows)
    png = (b"\x89PNG\r\n\x1a\n"
           + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 6, 0, 0, 0))
           + chunk(b"IDAT", zlib.compress(body, 9))
           + chunk(b"IEND", b""))
    open(path, "wb").write(png)

def resample(w, h, rows, size):
    """Bilinear, on premultiplied alpha so edges do not pick up the colour of
    fully transparent pixels."""
    out = []
    for oy in range(size):
        sy = (oy + 0.5) * h / size - 0.5
        y0 = max(0, min(h - 1, int(sy // 1))); y1 = min(h - 1, y0 + 1)
        fy = max(0.0, min(1.0, sy - y0))
        line = bytearray(size * 4)
        for ox in range(size):
            sx = (ox + 0.5) * w / size - 0.5
            x0 = max(0, min(w - 1, int(sx // 1))); x1 = min(w - 1, x0 + 1)
            fx = max(0.0, min(1.0, sx - x0))
            acc = [0.0, 0.0, 0.0, 0.0]
            for (yy, wy) in ((y0, 1 - fy), (y1, fy)):
                r = rows[yy]
                for (xx, wx) in ((x0, 1 - fx), (x1, fx)):
                    o = xx * 4
                    a = r[o + 3] / 255.0
                    k = wy * wx
                    acc[0] += r[o] * a * k
                    acc[1] += r[o + 1] * a * k
                    acc[2] += r[o + 2] * a * k
                    acc[3] += a * k
            a = acc[3]
            o = ox * 4
            if a > 0:
                line[o] = min(255, round(acc[0] / a))
                line[o + 1] = min(255, round(acc[1] / a))
                line[o + 2] = min(255, round(acc[2] / a))
            line[o + 3] = min(255, round(a * 255))
        out.append(bytes(line))
    return out

CANVAS, ART = 512, 412
src = sys.argv[1]; dst = sys.argv[2]
w, h, rows = load(src)
art = resample(w, h, rows, ART)
pad = (CANVAS - ART) // 2
blank = bytes(CANVAS * 4)
out = []
for y in range(CANVAS):
    if y < pad or y >= pad + ART:
        out.append(blank); continue
    line = bytearray(blank)
    line[pad * 4:(pad + ART) * 4] = art[y - pad]
    out.append(bytes(line))
save(dst, CANVAS, CANVAS, out)
print(f"  {src} {w}x{h} (full bleed) -> {dst} {CANVAS}x{CANVAS}, artwork {ART} ({ART/CANVAS:.1%})")
