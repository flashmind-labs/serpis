"""What both promo compositors share: the 1080p stage, type, easing, camera paths."""

import math

import numpy as np
from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H, FPS = 1920, 1080, 30

SF = "/System/Library/Fonts/SFNS.ttf"
MONO = "/System/Library/Fonts/SFNSMono.ttf"
_fonts = {}


def font(size, weight=400, path=SF):
    key = (size, weight, path)
    if key not in _fonts:
        f = ImageFont.truetype(path, size)
        if path == SF:
            f.set_variation_by_axes([100, min(96, max(17, size * 0.75)), 400, weight])
        _fonts[key] = f
    return _fonts[key]



# ---------- easing and timing ----------

def clamp(x, a=0.0, b=1.0):
    return max(a, min(b, x))


def ease(x):  # ease-in-out cubic
    x = clamp(x)
    return 4 * x ** 3 if x < 0.5 else 1 - (-2 * x + 2) ** 3 / 2


def ease_out(x):
    x = clamp(x)
    return 1 - (1 - x) ** 3


def ramp(t, a, b):
    return ease((t - a) / (b - a)) if b > a else float(t >= a)


def lerp(a, b, x):
    return a + (b - a) * x



# ---------- stage ----------

LW, LH = 384, 216
yy, xx = np.mgrid[0:LH, 0:LW].astype(np.float32)
xx /= LW
yy /= LH
GLOWS = [  # colour, centre path, radius, strength
    ((99, 102, 241), lambda s: (0.22 + 0.06 * math.sin(s * 0.31), 0.18 + 0.05 * math.cos(s * 0.27)), 0.42, 0.55),
    ((168, 85, 247), lambda s: (0.82 + 0.05 * math.cos(s * 0.23), 0.30 + 0.07 * math.sin(s * 0.35)), 0.38, 0.42),
    ((34, 211, 238), lambda s: (0.55 + 0.10 * math.sin(s * 0.19), 1.05 + 0.04 * math.cos(s * 0.29)), 0.36, 0.22),
]
BASE = np.array([7, 8, 12], np.float32)
rng = np.random.default_rng(7)
GRAIN = (rng.standard_normal((H, W, 1)) * 1.6).astype(np.float32)
vig = 1 - 0.55 * np.clip(((xx - .5) ** 2 + (yy - .5) ** 2 * 1.4) * 1.6, 0, 1)


def stage(s, glow=1.0):
    img = np.tile(BASE, (LH, LW, 1))
    for col, path, r, k in GLOWS:
        cx, cy = path(s)
        d2 = ((xx - cx) * 1.6) ** 2 + (yy - cy) ** 2
        img += np.exp(-d2 / (2 * r * r * 0.35))[..., None] * np.array(col, np.float32) * k * 0.38 * glow
    img *= vig[..., None]
    big = Image.fromarray(np.clip(img, 0, 255).astype(np.uint8)).resize((W, H), Image.BICUBIC)
    a = np.asarray(big, np.float32) + GRAIN
    return Image.fromarray(np.clip(a, 0, 255).astype(np.uint8))



# ---------- text ----------

def text_layer(txt, size, weight, fill, track=0.0, path=SF):
    f = font(size, weight, path)
    w = int(f.getlength(txt) + abs(track) * len(txt) + size)
    layer = Image.new("RGBA", (w, int(size * 1.5)), (0, 0, 0, 0))
    d = ImageDraw.Draw(layer)
    if track == 0:
        d.text((size // 2, size * 0.2), txt, font=f, fill=fill)
    else:
        x = size // 2
        for ch in txt:
            d.text((x, size * 0.2), ch, font=f, fill=fill)
            x += f.getlength(ch) + track
    return layer.crop(layer.getbbox())


def gradient_fill(layer, c1, c2):
    a = layer.getchannel("A")
    g = np.zeros((layer.height, layer.width, 3), np.float32)
    k = np.linspace(0, 1, layer.width)[None, :, None]
    g[:] = np.array(c1) * (1 - k) + np.array(c2) * k
    out = Image.fromarray(g.astype(np.uint8)).convert("RGBA")
    out.putalpha(a)
    return out


_tl = {}


def cached(key, make):
    if key not in _tl:
        _tl[key] = make()
    return _tl[key]


def blit(img, layer, cx, top, alpha=1.0, dy=0.0, scale=1.0, anchor="center"):
    if alpha <= 0.003:
        return
    l = layer
    if scale != 1.0:
        l = l.resize((max(1, int(l.width * scale)), max(1, int(l.height * scale))), Image.BICUBIC)
    if alpha < 1:
        l = l.copy()
        l.putalpha(l.getchannel("A").point(lambda v: int(v * alpha)))
    x = cx - l.width / 2 if anchor == "center" else cx
    img.alpha_composite(l, (int(round(x)), int(round(top + dy))))


def camera(keys_, x):
    for (a, s0, fx0, fy0), (b, s1, fx1, fy1) in zip(keys_, keys_[1:]):
        if a <= x <= b:
            k = ease((x - a) / (b - a)) if b > a else 1
            return lerp(s0, s1, k), lerp(fx0, fx1, k), lerp(fy0, fy1, k)
    return keys_[-1][1:]


def wordmark(size):
    return cached(("word", size), lambda: gradient_fill(text_layer("serpis", size, 720, (255, 255, 255, 255), -size * 0.02), (255, 255, 255), (178, 170, 255)))


def encode(path, n, render):
    """Frames render(t) for t in [0, n/FPS) piped to ffmpeg as an H.264 MP4."""
    import subprocess
    ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
                           "-c:v", "libx264", "-preset", "slow", "-crf", "15", "-pix_fmt", "yuv420p", "-movflags", "+faststart", path], stdin=subprocess.PIPE)
    for i in range(n):
        ff.stdin.write(render(i / FPS).tobytes())
        if i % 150 == 0:
            print(f"{i}/{n}", flush=True)
    ff.stdin.close()
    ff.wait()
