# Promo video

`build.sh` makes `target/promo/serpis-promo.mp4`, a 40-second 1080p promo. Re-run it when the
UI changes. It needs macOS (the compositor uses the SF Pro fonts), ffmpeg and python3. It
makes its own venv with Pillow and NumPy in `target/promo`. It isn't part of the tests.

What it does:

1. `record.sh` runs a real serpis window in a throwaway home, on a small demo project
   (`ratelimit`). The chats use `demo_llm.py`, a scripted model that streams like a real one, so
   no keys, providers or your own data are involved. Under AeroSpace the window is floated so it
   can be sized.
2. `record.lua` drives the window through the scenes (the chat fixing a test, splits and a
   terminal, the palette, the agent changing the font). It saves a frame about every 50 ms
   (`frames/<ms>.png`) and marks scenes and key presses in `marks.txt`.
3. `compose.py` puts the footage on an animated stage with headlines, camera moves, keycaps,
   a feature grid and the intro and outro. It pipes the frames to ffmpeg.

To change the copy, the pacing or the camera, edit `SEGMENTS` and `FEATURES` in `compose.py`
and run `build.sh --compose`, which reuses the last take. To check a few moments without
encoding, run
`target/promo/venv/bin/python scripts/promo/compose.py target/promo/take /tmp/p --preview 5,12.5`.
What the agent says is in `demo_llm.py`, and what is typed and pressed is in `record.lua`.
Both need a new take.

The video itself isn't committed. Publish it as a release asset or on the website.
