"""Composes the phone promo: the simulator recording (ios-record.sh) in a drawn iPhone on the
animated stage, with headlines beside it, camera moves, a feature grid and an outro.

    python compose_ios.py <take dir> <out.mp4> [--preview t1,t2,...]
"""

import os
import subprocess
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFilter

from stagekit import *  # noqa: F401,F403 (the stage, type, easing)

TAKE, OUT = sys.argv[1], sys.argv[2]
PREVIEW = [float(x) for x in sys.argv[4].split(",")] if len(sys.argv) > 4 and sys.argv[3] == "--preview" else None

marks = {}
for line in open(os.path.join(TAKE, "marks.txt")):
    ms, name = line.split()
    marks[name] = int(ms) / 1000

# ---------- footage ----------
# The simulator writes a frame only when the screen changes: each moment shows the last frame
# at or before it, by the recording's own timestamps (ffmpeg's fps filter misplaces them here).
SW, SH = 603, 1311


class Footage:
    def __init__(self):
        path = os.path.join(TAKE, "screen.mov")
        out = subprocess.run(["ffprobe", "-v", "error", "-select_streams", "v", "-show_entries", "frame=pts_time", "-of", "csv=p=0", path],
                             capture_output=True, text=True).stdout
        self.pts = [float(x.strip(",")) for x in out.split() if x.strip(",")]
        self.path = path
        self.proc = None
        self.index = -1
        self.frame = None

    def at(self, t):
        import bisect
        want = max(0, bisect.bisect_right(self.pts, t) - 1)
        if self.proc is None or want < self.index:
            if self.proc:
                self.proc.kill()
            self.proc = subprocess.Popen(["ffmpeg", "-v", "fatal", "-i", self.path, "-fps_mode", "passthrough", "-vf", f"scale={SW}:{SH}:flags=lanczos",
                                          "-f", "rawvideo", "-pix_fmt", "rgb24", "-"], stdout=subprocess.PIPE)
            self.index = -1
        while self.index < want:
            raw = self.proc.stdout.read(SW * SH * 3)
            if len(raw) < SW * SH * 3:
                break
            self.frame = Image.frombytes("RGB", (SW, SH), raw)
            self.index += 1
        return self.frame


footage = Footage()

# ---------- the phone ----------
BEZEL, EDGE = 18, 3
SCREEN_R = 0.135 * SW  # the screen's corner radius
PW, PH = SW + 2 * BEZEL, SH + 2 * BEZEL
PAD = 120
screen_mask = Image.new("L", (SW, SH), 0)
ImageDraw.Draw(screen_mask).rounded_rectangle((0, 0, SW - 1, SH - 1), SCREEN_R, fill=255)


def _phone_base():
    """The body (shadow, glow, frame) with a hole for the screen, at footage resolution."""
    w, h = PW + 2 * PAD, PH + 2 * PAD
    base = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    glow = Image.new("L", (w, h), 0)
    ImageDraw.Draw(glow).rounded_rectangle((PAD - 30, PAD - 10, PAD + PW + 30, PAD + PH + 30), SCREEN_R + 50, fill=70)
    tint = Image.new("RGBA", (w, h), (110, 110, 255, 0))
    tint.putalpha(glow.filter(ImageFilter.GaussianBlur(70)))
    base.alpha_composite(tint)
    sh = Image.new("L", (w, h), 0)
    ImageDraw.Draw(sh).rounded_rectangle((PAD + 10, PAD + 40, PAD + PW - 10, PAD + PH + 50), SCREEN_R + BEZEL, fill=190)
    shadow = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    shadow.putalpha(sh.filter(ImageFilter.GaussianBlur(40)))
    base.alpha_composite(shadow)
    d = ImageDraw.Draw(base)
    r = SCREEN_R + BEZEL
    # Titanium edge, then the black bezel.
    d.rounded_rectangle((PAD, PAD, PAD + PW - 1, PAD + PH - 1), r, fill=(92, 94, 104, 255))
    d.rounded_rectangle((PAD + 1, PAD + 1, PAD + PW - 2, PAD + PH - 2), r - 1, fill=(150, 152, 162, 255))
    d.rounded_rectangle((PAD + EDGE, PAD + EDGE, PAD + PW - 1 - EDGE, PAD + PH - 1 - EDGE), r - EDGE, fill=(10, 10, 12, 255))
    # Side buttons.
    for y0, y1, left in [(0.20, 0.25, True), (0.29, 0.36, True), (0.38, 0.45, True), (0.30, 0.41, False)]:
        x = PAD - 3 if left else PAD + PW - 1
        d.rounded_rectangle((x, PAD + PH * y0, x + 4, PAD + PH * y1), 2, fill=(120, 122, 132, 255))
    return base


PHONE = _phone_base()
ISLAND = Image.new("RGBA", (SW, SH), (0, 0, 0, 0))
iw, ih = SW * 0.31, SW * 0.088
ImageDraw.Draw(ISLAND).rounded_rectangle((SW / 2 - iw / 2, SW * 0.028, SW / 2 + iw / 2, SW * 0.028 + ih), ih / 2, fill=(0, 0, 0, 255))
_card = [None, None]


def phone(src):
    if _card[0] is not src:
        c = PHONE.copy()
        s = src.convert("RGBA")
        s.alpha_composite(ISLAND)
        s.putalpha(screen_mask)
        c.alpha_composite(s, (PAD + BEZEL, PAD + BEZEL))
        _card[0], _card[1] = src, c
    return _card[1]


BASE_SCALE = 0.72           # the phone at rest: about 945 px of screen height
CX, CY = 1340, 552          # its centre on the stage


def place_phone(img, src, scale, fx, fy, alpha=1.0, dx=0.0, dy=0.0):
    """As place_card in compose.py: screen point (fx, fy) held where it is at scale 1."""
    c = phone(src)
    s = BASE_SCALE * scale
    x1 = CX + (fx - .5) * SW * BASE_SCALE
    y1 = CY + (fy - .5) * SH * BASE_SCALE
    px = x1 - (PAD + BEZEL + fx * SW) * s + dx
    py = y1 - (PAD + BEZEL + fy * SH) * s + dy
    left, top = max(0, math.floor(px)), max(0, math.floor(py))
    right, bottom = min(W, math.ceil(px + c.width * s)), min(H, math.ceil(py + c.height * s))
    if right <= left or bottom <= top:
        return
    out = c.convert("RGBa").transform((right - left, bottom - top), Image.AFFINE,
                                      (1 / s, 0, (left - px) / s, 0, 1 / s, (top - py) / s), resample=Image.BICUBIC).convert("RGBA")
    if alpha < 1:
        out.putalpha(out.getchannel("A").point(lambda v: int(v * alpha)))
    img.alpha_composite(out, (left, top))


# ---------- the story ----------
# (name, src start, src end, speed, step, title, subtitle, camera keys [(fraction, scale, fx, fy)])
allow = marks["allow"]
SEGMENTS = [
    ("list", marks["list"], marks["ask"], 1.0, "01",
     "Every chat, in your pocket.", "From every machine you work on,\nwith where each one runs.",
     [(0, 1.0, .5, .5), (1, 1.03, .5, .3)]),
    ("ask", marks["ask"], marks["work"], 1.35, "02",
     "Start one anywhere.", "Pick a folder on any of your machines,\nask, and watch it work.",
     [(0, 1.03, .5, .3), (0.3, 1.0, .5, .5), (1, 1.0, .5, .5)]),
    ("work", marks["work"], marks["approve"], 1.0, "03",
     "See exactly what it did.", "Every step it took,\ndown to the diff.",
     [(0, 1.0, .5, .5), (0.25, 1.32, .5, .2), (0.9, 1.32, .5, .2), (1, 1.0, .5, .5)]),
    ("approve", marks["approve"], allow - 3.2, 1.7, "04",
     "Say yes from anywhere.", "When Claude Code asks before it acts,\nyou answer from your phone.",
     [(0, 1.0, .5, .5), (0.7, 1.0, .5, .5), (1, 1.3, .5, .86)]),
    ("approve", allow - 3.2, marks["end"], 1.0, "04",
     "Say yes from anywhere.", "When Claude Code asks before it acts,\nyou answer from your phone.",
     [(0, 1.3, .5, .86), (1, 1.32, .5, .82)]),
]

INTRO, GRID, OUTRO = 3.4, 6.5, 4.0
timeline = [(0.0, INTRO, "intro", None)]
t = INTRO
for seg in SEGMENTS:
    dur = (seg[2] - seg[1]) / seg[3]
    timeline.append((t, t + dur, "seg", seg))
    t += dur
timeline.append((t, t + GRID, "grid", None))
t += GRID
timeline.append((t, t + OUTRO, "outro", None))
TOTAL = t + OUTRO
seg_entries = [x for x in timeline if x[2] == "seg"]


def left_text(img, step, title, sub, alpha, dy):
    if alpha <= 0.003:
        return
    x = 200
    st = cached(("step", step), lambda: gradient_fill(text_layer(step, 26, 700, (255, 255, 255, 255), 2.0), (165, 170, 255), (205, 160, 255)))
    tl = cached(("t", title), lambda: text_layer(title, 62, 700, (246, 246, 250, 255), -0.8))
    blit(img, st, x, 390, alpha, dy * 0.6, anchor="left")
    blit(img, tl, x, 432, alpha, dy, anchor="left")
    y = 432 + tl.height + 26
    for i, line in enumerate(sub.split("\n")):
        sl = cached(("s", line), lambda: text_layer(line, 28, 420, (160, 163, 182, 255)))
        blit(img, sl, x, y + i * 40, alpha * 0.95, dy * 1.3, anchor="left")


def draw_segment(img, t, start, end, seg, idx):
    name, s0, s1, speed, step, title, sub, cam = seg
    local, dur = t - start, end - start
    sc, fx, fy = camera(cam, local / dur)
    alpha, dx, dy = 1.0, 0.0, 0.0
    if idx == 0:
        k = ease_out(local / 1.0)
        alpha, dy, sc = clamp(local / 0.5), (1 - k) * 220, sc * (0.95 + 0.05 * k)
    if idx == len(SEGMENTS) - 1:
        k = ramp(local, dur - 0.6, dur)
        alpha *= 1 - k
        dy -= k * 40
    place_phone(img, footage.at(s0 + local * speed), sc, fx, fy, alpha, dx, dy)
    # The headline: in when its scene starts, out when it ends; a scene continued keeps it.
    prev_same = idx > 0 and seg_entries[idx - 1][3][5] == title
    next_same = idx + 1 < len(seg_entries) and seg_entries[idx + 1][3][5] == title
    a_in = 1.0 if prev_same else ramp(local, 0.2, 0.7)
    a_out = 1.0 if next_same else 1 - ramp(local, dur - 0.4, dur)
    hdy = 0.0 if prev_same else (1 - ease_out((local - 0.2) / 0.7)) * 22
    left_text(img, step, title, sub, a_in * a_out, hdy)


FEATURES = [
    ("Pair in seconds", "Scan the code serpis shows on your\ncomputer, and its chats are here."),
    ("Live as it happens", "Answers, thinking and command\noutput stream in as they run."),
    ("Steer it, or stop it", "What you send while it works\nchanges its course."),
    ("Background work", "See what runs on the side,\nand stop it from the phone."),
    ("Usage limits", "Know when a limit hits, and let\nthe chat go on when it resets."),
    ("Your machines", "Your laptop, your desktop, your\nservers: reached over SSH."),
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
        g = Image.new("L", l.size, 0)
        ImageDraw.Draw(g).ellipse((pad - 40, pad - 60, pad + 160, pad + 100), fill=90)
        from PIL import ImageChops
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
    title = cached("gt", lambda: text_layer("All of serpis, wherever you are.", 54, 680, (246, 246, 250, 255), -0.6))
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


def draw_intro(img, t):
    wm = wordmark(168)
    a = ramp(t, 0.15, 0.9) * (1 - ramp(t, 2.55, 3.2))
    sc = 0.94 + 0.06 * ease_out((t - 0.15) / 1.2) + 0.05 * ramp(t, 2.55, 3.3)
    blit(img, wm, W / 2, 360, a, (1 - ease_out((t - 0.15) / 1.0)) * 26, sc)
    tag = cached("tag", lambda: text_layer("for iPhone", 40, 560, (190, 186, 255, 255)))
    blit(img, tag, W / 2, 580, ramp(t, 0.6, 1.2) * (1 - ramp(t, 2.45, 3.0)), (1 - ease_out((t - 0.6) / 0.9)) * 16)


def draw_outro(img, t):
    wm = wordmark(150)
    blit(img, wm, W / 2, 340, ramp(t, 0.1, 0.8), (1 - ease_out((t - 0.1) / 0.9)) * 22, 0.96 + 0.04 * ease_out(t / 1.2))
    tag = cached("tag", lambda: text_layer("for iPhone", 40, 560, (190, 186, 255, 255)))
    blit(img, tag, W / 2, 545, ramp(t, 0.4, 1.0), (1 - ease_out((t - 0.4) / 0.8)) * 14)
    line = cached("line", lambda: text_layer("Your agents keep working. Now you can keep up.", 30, 450, (170, 172, 190, 255)))
    blit(img, line, W / 2, 640, ramp(t, 0.9, 1.6), (1 - ease_out((t - 0.9) / 0.8)) * 12)


def render(t):
    img = stage(t).convert("RGBA")
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
    k = min(ramp(t, 0, 0.5), 1 - ramp(t, TOTAL - 0.6, TOTAL))
    rgb = img.convert("RGB")
    if k < 1:
        rgb = Image.blend(Image.new("RGB", (W, H)), rgb, k)
    return rgb


if PREVIEW:
    for p in PREVIEW:
        render(p).save(f"{OUT}-{p:05.2f}.png")
    print(f"total {TOTAL:.1f}s; timeline:", [(round(a, 1), k, d[0] if d else "") for a, _, k, d in timeline])
    sys.exit()

encode(OUT, int(TOTAL * FPS), render)
print("done", OUT, f"{TOTAL:.1f}s")
