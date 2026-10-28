# Promo videos

`build.sh` makes `target/promo/serpis-promo.mp4`, a 42-second 1080p promo of the desktop app.
`build.sh --ios` makes `target/promo/serpis-ios-promo.mp4`, a 56-second one of the iPhone
app. Re-run them when the UI changes. They need macOS (the compositors use the SF Pro fonts),
ffmpeg and python3, plus Xcode, xcodegen and an iPhone simulator for the phone one. They make
their own venv with Pillow and NumPy in `target/promo`. They aren't part of the tests.

Both run on a small demo project (`project.sh` makes it: `ratelimit`, a Rust crate with a flaky
test), with chats on `demo_llm.py`, a scripted model that streams like a real one. No keys,
providers or your own data are involved.

## Desktop

1. `record.sh` runs a real serpis window in a throwaway home. Under AeroSpace the window is
   floated so it can be sized.
2. `record.lua` drives the window through the scenes (the chat fixing a test, splits and a
   terminal, the palette, the agent changing the font). It saves a frame about every 50 ms
   (`frames/<ms>.png`) and marks scenes and key presses in `marks.txt`.
3. `compose.py` puts the footage on an animated stage with headlines, camera moves, keycaps,
   a feature grid and the intro and outro. It pipes the frames to ffmpeg.

## iPhone

1. `ios-record.sh` starts the phone tests' machine (`ios/test-machine.sh`) with the demo model,
   the demo project in its home, and Claude Code's stand-in in its demo mode
   (`FAKE_CLAUDE_DEMO`: a real-looking model name and edit). It sets the simulator to dark
   mode with a 9:41 status bar, and records its screen (`screen.mov`) while the
   `PromoRecording` UI test (`ios/SerpisUITests/PromoRecording.swift`) drives the app. That
   test marks each scene in `marks.txt`.
2. `compose_ios.py` puts the recording in a drawn iPhone beside the headlines, on the same
   stage (`stagekit.py` is what the two compositors share).

## Changing them

To change the copy, the pacing or the camera, edit `SEGMENTS` and `FEATURES` in the compositor
and run `build.sh [--ios] --compose`, which reuses the last take. To check a few moments
without encoding, run
`target/promo/venv/bin/python scripts/promo/compose.py target/promo/take /tmp/p --preview 5,12.5`
(or `compose_ios.py target/promo/ios-take`). What the agent says is in `demo_llm.py`, and what
is typed and tapped is in `record.lua` or `PromoRecording.swift`. Both need a new take.

The videos themselves aren't committed. Publish them as release assets or on the website.
