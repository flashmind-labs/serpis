"""Scripted, streaming OpenAI-compatible model for the serpis promo recording.

Each scene is picked by a keyword in the last user message; its steps run one per request
(the number of tool results since that message picks the step).
"""

import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file = sys.argv[1]

LIMITER_FIX = {
    "path": "src/limiter.rs",
    "edits": [{
        "oldText": "        let now = Instant::now();\n        let elapsed = now - self.last;",
        "newText": "        let now = self.clock.now();\n        let elapsed = now.saturating_duration_since(self.last);",
    }],
}

SCENES = {
    "flaky": [
        ("think", "The test sleeps for real time and compares against a wall clock. Let me look at the limiter first."),
        ("tool", "read", {"path": "src/limiter.rs"}),
        ("tool", "bash", {"command": "rg -n 'sleep|Instant::now' src tests"}),
        ("tool", "edit", LIMITER_FIX),
        ("text",
         "Found it: `refill()` read the real clock, so the test raced the scheduler: on a busy CI box "
         "the 100 ms sleep could take longer and the bucket refilled one token too many.\n\n"
         "What I changed:\n\n"
         "- The limiter now takes a `Clock`, so tests drive time by hand\n"
         "- `saturating_duration_since` guards against a clock that steps back\n\n"
         "```rust\nlet mut clock = ManualClock::new();\nlet mut rl = Limiter::with_clock(5, &clock);\n"
         "clock.advance(Duration::from_millis(100));\nassert_eq!(rl.available(), 1);\n```\n\n"
         "The test is now deterministic: no sleeps, no flakes."),
    ],
    "font": [
        ("tool", "serpis_api", {"topic": "variables", "filter": "font-size"}),
        ("tool", "serpis_lua", {"code": "serpis.var.set(\"font-size\", 19)\nserpis.var.set(\"font-mono-size\", 18)\nreturn serpis.var.get(\"font-size\")"}),
        ("text", "Done: **font-size** is now 19 and **font-mono-size** 18, live. "
                 "Want me to save them to your `init.lua` too?"),
    ],
}


def text_of(content):
    if isinstance(content, str):
        return content
    return "".join(p.get("text", "") for p in content or [] if isinstance(p, dict))


def chunk(delta, finish=None):
    return {"id": "r", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}


def pieces(text, size=3):
    words = text.split(" ")
    for i in range(0, len(words), size):
        yield " ".join(words[i:i + size]) + (" " if i + size < len(words) else "")


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def send(self, obj):
        self.wfile.write(f"data: {json.dumps(obj)}\n\n".encode())
        self.wfile.flush()

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        messages = body.get("messages", [])
        last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=0)
        prompt = text_of(messages[last_user].get("content")) if messages else ""
        done = sum(1 for m in messages[last_user:] if m.get("role") == "tool")
        scene = next((s for k, s in SCENES.items() if k in prompt.lower()), None)
        steps = [s for s in scene if s[0] != "think"] if scene else [("text", "Sure.")]
        think = next((s[1] for s in scene or [] if s[0] == "think"), None) if done == 0 else None
        step = steps[min(done, len(steps) - 1)]

        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        time.sleep(0.35)
        if think:
            for p in pieces(think, 2):
                self.send(chunk({"role": "assistant", "reasoning_content": p}))
                time.sleep(0.05)
        finish = "stop"
        if step[0] == "tool":
            time.sleep(0.25)
            self.send(chunk({"role": "assistant", "tool_calls": [{
                "index": 0, "id": f"call_{done}", "type": "function",
                "function": {"name": step[1], "arguments": json.dumps(step[2])},
            }]}))
            finish = "tool_calls"
        else:
            for p in pieces(step[1]):
                self.send(chunk({"role": "assistant", "content": p}))
                time.sleep(0.035)
        self.send(chunk({}, finish))
        self.send({"id": "r", "object": "chat.completion.chunk", "choices": [],
                   "usage": {"prompt_tokens": 12400 + 900 * done, "completion_tokens": 180, "total_tokens": 12580 + 900 * done}})
        self.wfile.write(b"data: [DONE]\n\n")


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
