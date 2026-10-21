"""Serpis' logo and app icon, from one definition of the mark.

The mark is a meander that is also a snake: an S that thickens from a thin tail (the river's
source, below) to a head with an eye (above), in sun gold to a warm orange-red, the colors of
the Comunitat Valenciana's sun and oranges. The app icon puts it on a deep Mediterranean blue.

Writes, into assets/:
  serpis-mark.svg          the mark alone (for dark backgrounds)
  serpis-icon.svg          the app icon (macOS proportions: an 824 px body in a 1024 canvas)
  serpis-logo-dark.svg     mark + wordmark for dark backgrounds
  serpis-logo-light.svg    mark + wordmark for light backgrounds (deeper gold)
  serpis.png               the app icon at 1024 px (scripts/install-macos.sh makes the .icns)

The wordmark is "serpis" in Manrope Bold (SIL Open Font License), letter-spaced -0.03 em,
converted to outlines so it looks the same everywhere. Pure Python, no dependencies:

    python3 scripts/make-icon.py
"""

import math
import os
import struct
import zlib

# The mark, in a 0..100 box: the tapered body (tail at the bottom right, head at the top left),
# the head and the eye.
BODY = [
    (76.6, 86.3), (78.7, 83.0), (79.7, 79.6), (79.9, 76.2), (79.1, 72.9), (77.6, 69.7), (75.4, 66.8), (72.8, 64.1),
    (69.8, 61.6), (66.5, 59.2), (63.1, 57.0), (59.6, 55.0), (56.1, 53.2), (52.8, 51.5), (49.8, 49.9), (46.3, 48.0),
    (42.9, 46.1), (39.9, 44.1), (37.3, 42.2), (35.1, 40.3), (33.5, 38.5), (32.5, 36.9), (32.1, 35.7), (32.1, 34.9),
    (32.3, 34.2), (32.8, 33.3), (33.9, 32.0), (35.7, 30.1), (38.9, 27.7), (29.1, 16.3), (25.5, 19.7), (22.5, 23.6),
    (20.3, 27.6), (19.2, 31.9), (19.3, 36.3), (20.5, 40.2), (22.5, 43.6), (25.0, 46.4), (27.9, 48.9), (31.1, 51.0),
    (34.6, 53.0), (38.3, 54.8), (42.2, 56.5), (46.2, 58.1), (49.4, 59.2), (52.8, 60.5), (56.3, 61.8), (59.8, 63.3),
    (63.2, 64.9), (66.3, 66.6), (69.2, 68.4), (71.8, 70.4), (73.9, 72.4), (75.4, 74.5), (76.5, 76.8), (76.9, 79.4),
    (76.6, 82.3), (75.4, 85.7),
]
HEAD = (35.2, 21.2, 7.8)
EYE = (37.6, 19.3, 1.7)
# The gradient runs from the tail (y=86) to the head (y=16).
TAIL_Y, HEAD_Y = 86.0, 16.0

TILE = "#0f1d33"          # deep Mediterranean blue
GOLD, RED = "#f6b73c", "#e2452c"
GOLD_LIGHT, RED_LIGHT = "#d98e12", "#c4301c"   # on white, the bright gold would be too faint
TEXT_DARK, TEXT_LIGHT = "#e8ecf3", "#11141b"

# "serpis" in Manrope Bold, font units (2000 per em), baseline at y=0, letter-spaced -0.03 em.
WORDMARK_UPM, WORDMARK_WIDTH = 2000, 5661.0
WORDMARK = (
    "M557.3 29.7Q356.8 29.7 232.1 -60.2Q107.3 -150 80 -313.3L326 -351Q344.7 -271 410 -225.2Q475.3 -179.3 574.3 -179.3Q660.5 -179.3 708.3 -213.6Q756 -247.8 756 -308.3Q756 -344.8 738.3 -367.3Q720.7 -389.8 661.8 -410.8Q603 -431.8 481 -464.3Q345.3 -499.7 265.3 -541.3Q185.2 -582.8 150.2 -639.8Q115.3 -696.8 115.3 -778Q115.3 -879 168 -953.8Q220.7 -1028.5 316.1 -1069.3Q411.5 -1110 540.3 -1110Q666 -1110 763.6 -1070.9Q861.2 -1031.8 921.3 -960.1Q981.5 -888.3 994.7 -791.3L748.7 -747Q739.7 -815.8 686.1 -855.9Q632.5 -896 544.3 -901Q459.3 -905.5 407.2 -875.2Q355 -844.8 355 -790.3Q355 -758.5 376.1 -736.6Q397.2 -714.7 460.4 -693Q523.7 -671.3 650 -639.3Q779.5 -605.8 856.7 -563.4Q933.8 -521 968.2 -462.1Q1002.7 -403.2 1002.7 -319.3Q1002.7 -156.5 884.3 -63.4Q766 29.7 557.3 29.7Z "
    "M1653.7 30Q1489.8 30 1366 -41.1Q1242.2 -112.2 1172.6 -238.1Q1103 -364 1103 -528.3Q1103 -705.3 1171.2 -835.8Q1239.5 -966.3 1360.4 -1038.2Q1481.3 -1110 1640.3 -1110Q1807.8 -1110 1925.3 -1031.7Q2042.8 -953.3 2100.1 -810.3Q2157.3 -667.3 2143.2 -472.7H1904V-560.7Q1903 -737 1842 -818.2Q1781 -899.3 1650.3 -899.3Q1502.5 -899.3 1430.2 -807.7Q1358 -716 1358 -540Q1358 -375.7 1430.2 -285.7Q1502.5 -195.7 1640.3 -195.7Q1729.3 -195.7 1793.7 -235.3Q1858 -275 1893 -349.7L2131.3 -278Q2069.5 -131.7 1939.7 -50.8Q1810 30 1653.7 30ZM1282 -472.7V-655H2025.3V-472.7Z "
    "M2307 0V-1080H2520V-817L2494 -851Q2514.3 -905 2547.8 -949.7Q2581.3 -994.3 2628 -1023.3Q2667.8 -1050.5 2716.2 -1065.5Q2764.7 -1080.5 2815.7 -1084Q2866.7 -1087.5 2915.3 -1080V-855Q2866.7 -869.2 2804.6 -863.8Q2742.5 -858.5 2691.7 -829.3Q2642.2 -802.2 2610.7 -760.3Q2579.3 -718.3 2564.3 -664.8Q2549.3 -611.3 2549.3 -548.7V0Z "
    "M3563.7 30Q3405.7 30 3299 -45.2Q3192.3 -120.3 3138.2 -249.4Q3084 -378.5 3084 -540.3Q3084 -702.7 3137.8 -831.6Q3191.7 -960.5 3296.4 -1035.3Q3401.2 -1110 3554.7 -1110Q3708 -1110 3820.7 -1035.7Q3933.5 -961.5 3995.2 -832.9Q4057 -704.3 4057 -540.3Q4057 -378.3 3995.9 -249.3Q3934.8 -120.2 3824.1 -45.1Q3713.3 30 3563.7 30ZM3043.3 480V-1080H3256.3V-321.7H3285.7V480ZM3527 -185.7Q3621.2 -185.7 3681.9 -233Q3742.7 -280.3 3772.3 -360.8Q3802 -441.3 3802 -540.3Q3802 -638.3 3771.8 -718.7Q3741.5 -799 3678.8 -846.7Q3616 -894.3 3518.7 -894.3Q3426.8 -894.3 3368.8 -849.8Q3310.8 -805.3 3283.6 -725.4Q3256.3 -645.5 3256.3 -540.3Q3256.3 -435.5 3283.5 -355.4Q3310.7 -275.3 3370.3 -230.5Q3430 -185.7 3527 -185.7Z "
    "M4237 -1243.7V-1465H4477.7V-1243.7ZM4237 0V-1080H4477.7V0Z "
    "M5135.3 29.7Q4934.8 29.7 4810.1 -60.2Q4685.3 -150 4658 -313.3L4904 -351Q4922.7 -271 4988 -225.2Q5053.3 -179.3 5152.3 -179.3Q5238.5 -179.3 5286.3 -213.6Q5334 -247.8 5334 -308.3Q5334 -344.8 5316.3 -367.3Q5298.7 -389.8 5239.8 -410.8Q5181 -431.8 5059 -464.3Q4923.3 -499.7 4843.3 -541.3Q4763.2 -582.8 4728.2 -639.8Q4693.3 -696.8 4693.3 -778Q4693.3 -879 4746 -953.8Q4798.7 -1028.5 4894.1 -1069.3Q4989.5 -1110 5118.3 -1110Q5244 -1110 5341.6 -1070.9Q5439.2 -1031.8 5499.3 -960.1Q5559.5 -888.3 5572.7 -791.3L5326.7 -747Q5317.7 -815.8 5264.1 -855.9Q5210.5 -896 5122.3 -901Q5037.3 -905.5 4985.2 -875.2Q4933 -844.8 4933 -790.3Q4933 -758.5 4954.1 -736.6Q4975.2 -714.7 5038.4 -693Q5101.7 -671.3 5228 -639.3Q5357.5 -605.8 5434.7 -563.4Q5511.8 -521 5546.2 -462.1Q5580.7 -403.2 5580.7 -319.3Q5580.7 -156.5 5462.3 -63.4Q5344 29.7 5135.3 29.7Z"
)

# macOS icon grid: an 824 px body centered in a 1024 canvas, corner radius 185.
N, BODY_PX, RADIUS = 1024, 824, 185
OFFSET = (N - BODY_PX) / 2
SCALE = BODY_PX / 100

ASSETS = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "assets")


def mark_svg(gold, red, grad_id):
    """The mark's elements, in its 0..100 box (the eye is a hole)."""
    pts = " ".join(f"{x},{y}" for x, y in BODY)
    hx, hy, hr = HEAD
    ex, ey, er = EYE
    head = (f"M{hx - hr} {hy}a{hr} {hr} 0 1 0 {2 * hr} 0a{hr} {hr} 0 1 0 {-2 * hr} 0Z"
            f"M{ex - er} {ey}a{er} {er} 0 1 0 {2 * er} 0a{er} {er} 0 1 0 {-2 * er} 0Z")
    return (
        f'<defs><linearGradient id="{grad_id}" gradientUnits="userSpaceOnUse" x1="0" y1="{TAIL_Y}" x2="0" y2="{HEAD_Y}">'
        f'<stop offset="0" stop-color="{gold}"/><stop offset="1" stop-color="{red}"/></linearGradient></defs>'
        f'<polygon points="{pts}" fill="url(#{grad_id})"/>'
        f'<path d="{head}" fill="{red}" fill-rule="evenodd"/>'
    )


def write(name, text):
    with open(os.path.join(ASSETS, name), "w") as f:
        f.write(text)


def svgs():
    write("serpis-mark.svg",
          '<svg xmlns="http://www.w3.org/2000/svg" viewBox="14 10 72 80" role="img" aria-label="serpis">'
          + mark_svg(GOLD, RED, "g") + "</svg>\n")
    write("serpis-icon.svg",
          f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {N} {N}" role="img" aria-label="serpis">'
          f'<rect x="{OFFSET}" y="{OFFSET}" width="{BODY_PX}" height="{BODY_PX}" rx="{RADIUS}" fill="{TILE}"/>'
          f'<g transform="translate({OFFSET} {OFFSET}) scale({SCALE})">' + mark_svg(GOLD, RED, "g") + "</g></svg>\n")
    # The wordmark beside the mark: x-height about 45% of the mark's box, centered on it.
    s = 84 / WORDMARK_UPM
    baseline, x0 = 73.6, 91
    width = x0 + WORDMARK_WIDTH * s + 4
    for name, gold, red, text in [("serpis-logo-dark.svg", GOLD, RED, TEXT_DARK), ("serpis-logo-light.svg", GOLD_LIGHT, RED_LIGHT, TEXT_LIGHT)]:
        write(name,
              f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="14 8 {width - 14:.1f} 90" role="img" aria-label="serpis">'
              + mark_svg(gold, red, "g")
              + f'<path transform="translate({x0} {baseline}) scale({s:.4f})" d="{WORDMARK}" fill="{text}"/></svg>\n')


def hex_rgb(h):
    return tuple(int(h[i:i + 2], 16) for i in (1, 3, 5))


def mix(a, b, t):
    return tuple(a[i] + (b[i] - a[i]) * t for i in range(3))


def png():
    """The app icon, rasterized with anti-aliasing (4 samples per row, exact horizontal coverage)."""
    tile, gold, red = hex_rgb(TILE), hex_rgb(GOLD), hex_rgb(RED)
    poly = [(OFFSET + x * SCALE, OFFSET + y * SCALE) for x, y in BODY]
    edges = list(zip(poly, poly[1:] + poly[:1]))
    head = tuple(v * SCALE + (OFFSET if i < 2 else 0) for i, v in enumerate(HEAD))
    eye = tuple(v * SCALE + (OFFSET if i < 2 else 0) for i, v in enumerate(EYE))
    ymin, ymax = int(min(p[1] for p in poly)), int(max(p[1] for p in poly)) + 1

    def tile_cover(x, y):
        h = BODY_PX / 2 - RADIUS
        dx, dy = abs(x - N / 2) - h, abs(y - N / 2) - h
        d = math.hypot(max(dx, 0), max(dy, 0)) + min(max(dx, dy), 0) - RADIUS
        return min(max(0.5 - d, 0.0), 1.0)

    def circle_cover(c, x, y):
        return min(max(c[2] - math.hypot(x - c[0], y - c[1]) + 0.5, 0.0), 1.0)

    rows = []
    for py in range(N):
        cover = [0.0] * N
        if ymin <= py <= ymax:
            for sub in range(4):
                sy = py + (sub + 0.5) / 4
                xs = sorted(x0 + (sy - y0) * (x1 - x0) / (y1 - y0) for (x0, y0), (x1, y1) in edges if (y0 <= sy < y1) or (y1 <= sy < y0))
                for xa, xb in zip(xs[::2], xs[1::2]):
                    for px in range(max(int(xa), 0), min(int(xb) + 1, N)):
                        cover[px] += 0.25 * max(0.0, min(px + 1, xb) - max(px, xa))
        t = min(max((TAIL_Y - (py + 0.5 - OFFSET) / SCALE) / (TAIL_Y - HEAD_Y), 0.0), 1.0)
        body_color = mix(gold, red, t)
        row = bytearray([0])
        for px in range(N):
            x, y = px + 0.5, py + 0.5
            a = tile_cover(x, y)
            if a <= 0:
                row += bytes(4)
                continue
            c = tile
            b = min(cover[px], 1.0)
            if b > 0:
                c = mix(c, body_color, b)
            hc = circle_cover(head, x, y)
            if hc > 0:
                c = mix(c, red, hc)
                ec = circle_cover(eye, x, y)
                if ec > 0:
                    c = mix(c, tile, ec)
            row += bytes(round(v) for v in c) + bytes([round(255 * a)])
        rows.append(bytes(row))

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data) & 0xFFFFFFFF)

    data = b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", N, N, 8, 6, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(b"".join(rows), 9)) + chunk(b"IEND", b"")
    with open(os.path.join(ASSETS, "serpis.png"), "wb") as f:
        f.write(data)


if __name__ == "__main__":
    os.makedirs(ASSETS, exist_ok=True)
    svgs()
    png()
    for n in ["serpis-mark.svg", "serpis-icon.svg", "serpis-logo-dark.svg", "serpis-logo-light.svg", "serpis.png"]:
        print(os.path.normpath(os.path.join(ASSETS, n)))
