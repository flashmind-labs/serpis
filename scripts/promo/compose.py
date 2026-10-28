"""Composes the serpis promo: recorded footage (record.sh) on an animated stage, with titles,
camera moves, keycaps, a feature grid and an outro. Pipes frames to ffmpeg.

    python compose.py <take dir> <out.mp4> [--preview t1,t2,...]

--preview renders stills at those times (<out.mp4>-<t>.png) instead of the video.
"""

import bisect
import math
import os
import subprocess
import sys

import numpy as np
from PIL import Image, ImageChops, ImageDraw, ImageFilter, ImageFont

W, H, FPS = 1920, 1080, 30
TAKE, OUT = sys.argv[1], sys.argv[2]
PREVIEW = [float(x) for x in sys.argv[4].split(",")] if len(sys.argv) > 4 and sys.argv[3] == "--preview" else None

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


# ---------- footage ----------

frames_dir = os.path.join(TAKE, "frames")
frame_ms = sorted(int(f[:-4]) for f in os.listdir(frames_dir) if f.endswith(".png"))
marks, keys = {}, []
for line in open(os.path.join(TAKE, "marks.txt")):
    parts = line.split()
    if parts[1] == "key":
        keys.append((int(parts[0]), parts[2]))
    else:
        marks[parts[1]] = int(parts[0])
_last = [None, None]


def footage(ms):
    i = max(0, bisect.bisect_right(frame_ms, ms) - 1)
    if _last[0] != frame_ms[i]:
        _last[0] = frame_ms[i]
        _last[1] = Image.open(os.path.join(frames_dir, f"{frame_ms[i]:07d}.png")).convert("RGB")
    return _last[1]


# Footage segments: (name, src start, src end, speed, title, subtitle, camera keys)
# camera keys: [(fraction of segment, scale, focus x, focus y)], focus in window fractions.
SEGMENTS = [
    ("chat", marks["chat"] + 200, marks["windows"], 1.2,
     "Agents that do the work.", "Any model, with real tools. Every step streams in as it runs.",
     [(0, 1.0, .5, .5), (0.55, 1.0, .5, .5), (0.8, 1.18, .58, .62), (1, 1.18, .58, .62)]),
    ("windows", marks["windows"], marks["palette"], 1.3,
     "Tile everything.", "Chats, terminals and files side by side, on workspaces, like i3.",
     [(0, 1.0, .5, .5), (1, 1.04, .62, .5)]),
    ("palette", marks["palette"], marks["extend"], 1.0,
     "Every action is a keystroke away.", "One palette for every command, fuzzy-searched.",
     [(0, 1.04, .55, .4), (0.3, 1.2, .5, .3), (0.85, 1.2, .5, .3), (1, 1.0, .5, .5)]),
    ("extend", marks["extend"], marks["end"] - 600, 1.1,
     "Reshape it by asking.", "Agents read serpis's own Lua API and change it, live.",
     [(0, 1.0, .5, .5), (0.25, 1.0, .5, .5), (0.5, 1.32, .8, .93), (1, 1.36, .8, .93)]),
]

INTRO, GRID, OUTRO = 3.4, 6.5, 4.0
BASE_SCALE = 0.9
CARD_CY = 642  # card centre at scale 1

timeline = []  # (start, end, kind, data)
t = 0.0
timeline.append((t, t + INTRO, "intro", None))
t += INTRO
for seg in SEGMENTS:
    dur = (seg[2] - seg[1]) / 1000 / seg[3]
    timeline.append((t, t + dur, "seg", seg))
    t += dur
timeline.append((t, t + GRID, "grid", None))
t += GRID
timeline.append((t, t + OUTRO, "outro", None))
TOTAL = t + OUTRO
FOOT_START, FOOT_END = timeline[1][0], timeline[len(SEGMENTS)][1]

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


# ---------- the window card ----------

FW, FH = Image.open(os.path.join(frames_dir, f"{frame_ms[0]:07d}.png")).size
PAD, RAD = 110, 16
mask = Image.new("L", (FW, FH), 0)
ImageDraw.Draw(mask).rounded_rectangle((0, 0, FW - 1, FH - 1), RAD, fill=255)
shadow = Image.new("RGBA", (FW + 2 * PAD, FH + 2 * PAD), (0, 0, 0, 0))
sh = Image.new("L", shadow.size, 0)
ImageDraw.Draw(sh).rounded_rectangle((PAD - 6, PAD + 22, PAD + FW + 6, PAD + FH + 40), RAD + 10, fill=170)
sh = sh.filter(ImageFilter.GaussianBlur(38))
shadow.putalpha(sh)
glow = Image.new("L", shadow.size, 0)
ImageDraw.Draw(glow).rounded_rectangle((PAD - 30, PAD - 30, PAD + FW + 30, PAD + FH + 30), RAD + 30, fill=60)
glow = glow.filter(ImageFilter.GaussianBlur(60))
glow_l = Image.new("RGBA", shadow.size, (110, 110, 255, 0))
glow_l.putalpha(glow)
CARD_BASE = Image.alpha_composite(glow_l, shadow)
border = Image.new("RGBA", (FW, FH), (0, 0, 0, 0))
bd = ImageDraw.Draw(border)
bd.rounded_rectangle((0, 0, FW - 1, FH - 1), RAD, outline=(255, 255, 255, 34), width=1)
_card = [None, None]


def card(src):
    if _card[0] is not src:
        c = CARD_BASE.copy()
        win = src.convert("RGBA")
        win.putalpha(mask)
        win = Image.alpha_composite(win, border)
        c.alpha_composite(win, (PAD, PAD))
        _card[0], _card[1] = src, c
    return _card[1]


def place_card(img, src, scale, fx, fy, alpha=1.0, dy=0.0):
    """Card at `scale` (of BASE_SCALE), with window point (fx, fy) moved toward the stage point
    it has at scale 1 — a camera push toward it."""
    c = card(src)
    s = BASE_SCALE * scale
    cw, ch = int(c.width * s), int(c.height * s)
    cs = c.resize((cw, ch), Image.LANCZOS if s < 1 else Image.BICUBIC)
    # Where the focus point sits at scale 1, and keep it there.
    x1 = W / 2 + (fx - .5) * FW * BASE_SCALE
    y1 = CARD_CY + (fy - .5) * FH * BASE_SCALE
    px = x1 - (PAD + fx * FW) * s
    py = y1 - (PAD + fy * FH) * s + dy
    if alpha < 1:
        a = cs.getchannel("A").point(lambda v: int(v * alpha))
        cs.putalpha(a)
    img.alpha_composite(cs, (int(round(px)), int(round(py))))


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


def headline(img, title, sub, alpha, dy):
    tl = cached(("t", title), lambda: text_layer(title, 50, 680, (246, 246, 250, 255), -0.6))
    sl = cached(("s", sub), lambda: text_layer(sub, 24, 420, (160, 163, 182, 255)))
    blit(img, tl, W / 2, 58, alpha, dy)
    blit(img, sl, W / 2, 58 + tl.height + 20, alpha * 0.95, dy * 1.3)


def keycap(img, label, age):
    if age < 0 or age > 1.3:
        return
    a = ramp(age, 0, 0.12) * (1 - ramp(age, 1.0, 1.3))
    pop = 0.85 + 0.15 * ease_out(age / 0.18)
    f = font(34, 560)
    tw = f.getlength(label)
    w, h = int(tw + 56), 66
    layer = cached(("key", label), lambda: _keycap_layer(label, w, h, f))
    blit(img, layer, W / 2, H - 120, a, (1 - ease_out(age / 0.25)) * 14, pop)


def _keycap_layer(label, w, h, f):
    pad = 30
    l = Image.new("RGBA", (w + 2 * pad, h + 2 * pad), (0, 0, 0, 0))
    s = Image.new("L", l.size, 0)
    ImageDraw.Draw(s).rounded_rectangle((pad, pad + 8, pad + w, pad + h + 8), 18, fill=150)
    l.putalpha(s.filter(ImageFilter.GaussianBlur(14)))
    d = ImageDraw.Draw(l)
    d.rounded_rectangle((pad, pad, pad + w, pad + h), 18, fill=(28, 29, 40, 238), outline=(255, 255, 255, 40), width=1)
    d.text((pad + w / 2, pad + h / 2 + 1), label, font=f, fill=(236, 237, 245, 255), anchor="mm")
    return l


# ---------- scenes ----------

def camera(keys_, x):
    for (a, s0, fx0, fy0), (b, s1, fx1, fy1) in zip(keys_, keys_[1:]):
        if a <= x <= b:
            k = ease((x - a) / (b - a)) if b > a else 1
            return lerp(s0, s1, k), lerp(fx0, fx1, k), lerp(fy0, fy1, k)
    return keys_[-1][1:]


WORD = None


def wordmark(size):
    return cached(("word", size), lambda: gradient_fill(text_layer("serpis", size, 720, (255, 255, 255, 255), -size * 0.02), (255, 255, 255), (178, 170, 255)))


def draw_intro(img, t):
    wm = wordmark(168)
    a = ramp(t, 0.15, 0.9) * (1 - ramp(t, 2.55, 3.2))
    rise = (1 - ease_out((t - 0.15) / 1.0)) * 26
    sc = 0.94 + 0.06 * ease_out((t - 0.15) / 1.2) + 0.05 * ramp(t, 2.55, 3.3)
    blit(img, wm, W / 2, 380, a, rise, sc)
    tag = cached("tag", lambda: text_layer("The agent harness you can reshape.", 34, 450, (170, 172, 190, 255)))
    blit(img, tag, W / 2, 600, ramp(t, 0.7, 1.4) * (1 - ramp(t, 2.45, 3.0)), (1 - ease_out((t - 0.7) / 0.9)) * 16)


def draw_segment(img, t, start, end, seg, idx):
    name, s0, s1, speed, title, sub, cam = seg
    local = t - start
    dur = end - start
    src = s0 + local * 1000 * speed
    x = local / dur
    sc, fx, fy = camera(cam, x)
    # The card rises in after the intro, and leaves before the grid.
    alpha, dy = 1.0, 0.0
    if idx == 0:
        k = ease_out(local / 0.9)
        alpha, dy, sc = clamp(local / 0.5), (1 - k) * 160, sc * (0.96 + 0.04 * k)
    if idx == len(SEGMENTS) - 1:
        k = ramp(local, dur - 0.6, dur)
        alpha *= 1 - k
        dy -= k * 30
    place_card(img, footage(src), sc, fx, fy, alpha, dy)
    # Headline: in, out when the camera pushes in (it would cover it), or at the segment's end.
    zoomed = clamp((sc - 1.06) / 0.08)
    ha = ramp(local, 0.15, 0.6) * (1 - ramp(local, dur - 0.35, dur)) * (1 - zoomed)
    hdy = (1 - ease_out((local - 0.15) / 0.6)) * 18
    if ha > 0:
        # A soft dark band keeps it legible over the card when they overlap.
        headline(img, title, sub, ha, hdy)
    for ms, label in keys:
        if s0 <= ms < s1:
            keycap(img, label, local - (ms - s0) / 1000 / speed)


FEATURES = [
    ("Claude Code, built in", "Your Claude subscription, inside serpis's\nwindows, sidebar and phone app."),
    ("Any machine", "Chats and terminals run on your\nservers, reached over SSH."),
    ("iPhone app", "Follow, steer and approve your\nagents from anywhere."),
    ("Skills", "pi-compatible skills for every agent,\nloaded on demand."),
    ("Notifications", "Know the moment an agent finishes\nor needs you."),
    ("Scriptable to the core", "Every behavior is a setting,\nevery action a Lua call."),
]
ACCENTS = [(129, 140, 248), (192, 132, 252), (56, 189, 248), (52, 211, 153), (251, 191, 36), (244, 114, 182)]
CW_, CH_ = 500, 196


def feature_card(i):
    def make():
        title, body = FEATURES[i]
        pad = 40
        l = Image.new("RGBA", (CW_ + 2 * pad, CH_ + 2 * pad), (0, 0, 0, 0))
        s = Image.new("L", l.size, 0)
        ImageDraw.Draw(s).rounded_rectangle((pad, pad + 14, pad + CW_, pad + CH_ + 14), 22, fill=120)
        l.putalpha(s.filter(ImageFilter.GaussianBlur(22)))
        d = ImageDraw.Draw(l)
        d.rounded_rectangle((pad, pad, pad + CW_, pad + CH_), 22, fill=(20, 21, 30, 235), outline=(255, 255, 255, 30), width=1)
        col = ACCENTS[i]
        # An accent glow in the corner and a dot.
        g = Image.new("L", l.size, 0)
        ImageDraw.Draw(g).ellipse((pad - 40, pad - 60, pad + 160, pad + 100), fill=90)
        g = ImageChops.multiply(g.filter(ImageFilter.GaussianBlur(40)), l.getchannel("A").point(lambda v: 255 if v > 200 else 0))
        tint = Image.new("RGBA", l.size, col + (0,))
        tint.putalpha(g)
        l.alpha_composite(tint)
        d = ImageDraw.Draw(l)
        d.ellipse((pad + 34, pad + 40, pad + 48, pad + 54), fill=col + (255,))
        d.text((pad + 62, pad + 30), title, font=font(30, 640), fill=(244, 244, 250, 255))
        d.multiline_text((pad + 34, pad + 86), body, font=font(23, 420), fill=(158, 161, 180, 255), spacing=10)
        return l
    return cached(("feat", i), make)


def draw_grid(img, t):
    title = cached("gt", lambda: text_layer("And that's not all.", 54, 680, (246, 246, 250, 255), -0.6))
    out = 1 - ramp(t, GRID - 0.5, GRID)
    blit(img, title, W / 2, 120, ramp(t, 0.1, 0.6) * out, (1 - ease_out((t - 0.1) / 0.6)) * 18)
    gx, gy = 36, 34
    total_w = 3 * CW_ + 2 * gx
    for i in range(6):
        r, c = divmod(i, 3)
        at = 0.45 + i * 0.12
        a = ramp(t, at, at + 0.45) * out
        x = W / 2 - total_w / 2 + c * (CW_ + gx) + CW_ / 2
        y = 290 + r * (CH_ + gy) - 40
        blit(img, feature_card(i), x, y, a, (1 - ease_out((t - at) / 0.6)) * 30, 0.97 + 0.03 * ease_out((t - at) / 0.6))


def draw_outro(img, t):
    wm = wordmark(150)
    a = ramp(t, 0.1, 0.8)
    blit(img, wm, W / 2, 360, a, (1 - ease_out((t - 0.1) / 0.9)) * 22, 0.96 + 0.04 * ease_out(t / 1.2))
    tag = cached("tag2", lambda: text_layer("The agent harness you can reshape.", 34, 450, (170, 172, 190, 255)))
    blit(img, tag, W / 2, 570, ramp(t, 0.5, 1.2), (1 - ease_out((t - 0.5) / 0.8)) * 14)
    fine = cached("fine", lambda: text_layer("Rust  ·  egui  ·  Lua  ·  macOS, Linux and iPhone", 22, 450, (110, 112, 132, 255), 1.0))
    blit(img, fine, W / 2, 660, ramp(t, 1.0, 1.7) * 0.9)


def render(t):
    fade_glow = 1.0
    img = stage(t, fade_glow).convert("RGBA")
    for i, (a, b, kind, data) in enumerate(timeline):
        if a <= t < b or (kind == "outro" and t >= a):
            if kind == "intro":
                draw_intro(img, t - a)
            elif kind == "seg":
                draw_segment(img, t, a, b, data, i - 1)
            elif kind == "grid":
                draw_grid(img, t - a)
            else:
                draw_outro(img, t - a)
    # Fade in from black and out to it.
    k = min(ramp(t, 0, 0.5), 1 - ramp(t, TOTAL - 0.6, TOTAL))
    rgb = img.convert("RGB")
    if k < 1:
        rgb = Image.blend(Image.new("RGB", (W, H)), rgb, k)
    return rgb


if PREVIEW:
    for p in PREVIEW:
        render(p).save(f"{OUT}-{p:05.2f}.png")
    print(f"total {TOTAL:.1f}s; timeline:", [(round(a, 1), k) for a, _, k, _ in timeline])
    sys.exit()

ff = subprocess.Popen(["ffmpeg", "-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "rgb24", "-s", f"{W}x{H}", "-r", str(FPS), "-i", "-",
                       "-c:v", "libx264", "-preset", "slow", "-crf", "15", "-pix_fmt", "yuv420p", "-movflags", "+faststart", OUT], stdin=subprocess.PIPE)
n = int(TOTAL * FPS)
for i in range(n):
    ff.stdin.write(render(i / FPS).tobytes())
    if i % 150 == 0:
        print(f"{i}/{n}", flush=True)
ff.stdin.close()
ff.wait()
print("done", OUT, f"{TOTAL:.1f}s")
