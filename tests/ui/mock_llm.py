"""A local OpenAI-compatible model for UI tests: no network, no keys.

Every response reports 1000 prompt tokens and 10 completion tokens (tests price them in
models.json). What it says depends on the last user message:

  THINK ...       think six lines, then reply "ok"
  TOOL ...        call bash `echo hi`
  CODE ...        reply with a fenced Rust code block
  INLINE ...      reply with inline code (Run `cargo test` now.)
  LONG ...        reply with a long answer (paragraphs, lists, code, a table)
  READ:<path>     call read on that path
  SHELL:<command> call bash with that command
  DESCRIBED:<description>|<command> the same, saying what it is for
  STEPS:<description>|<command>;;... several, one after another
  LUA:<code>      call serpis_lua with that code
  API:<topic>     call serpis_api with that topic
  EDIT:<path>|<old>|<new>   call edit on that file, replacing old with new
  anything else   reply "ok"

After a tool result it replies "done". Each request is logged as one JSON line to the file
given as the second argument: {"n", "last_role", "tool_content", "tools", "images", "text",
"skills", "serpis_docs"} (images: how many the last message carries; text: its text; skills:
whether the system prompt lists skills; serpis_docs: whether it points requests about serpis at
serpis_api).

  python3 mock_llm.py <port-file> <log-file>    (listens on a free port, written to port-file)
"""

import json
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file, log_file = sys.argv[1], sys.argv[2]
count = 0


def text_of(content):
    if isinstance(content, str):
        return content
    return "".join(p.get("text", "") for p in content or [] if isinstance(p, dict))


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        global count
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        messages = body.get("messages", [])
        last = messages[-1] if messages else {}
        count += 1
        with open(log_file, "a") as f:
            f.write(json.dumps({
                "n": count,
                "last_role": last.get("role"),
                "tool_content": text_of(last.get("content")) if last.get("role") == "tool" else None,
                "tools": [t["function"]["name"] for t in body.get("tools", [])],
                "images": sum(1 for p in (last.get("content") if isinstance(last.get("content"), list) else []) if isinstance(p, dict) and p.get("type") == "image_url"),
                "text": text_of(last.get("content"))[:4000],
                "skills": "<available_skills>" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "serpis_docs": "Call serpis_api first" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
            }) + "\n")

        delta, finish = {"role": "assistant", "content": "ok"}, "stop"
        # STEPS: one command after another in the same turn.
        users = [m for m in messages if m.get("role") == "user"]
        asked = text_of(users[-1].get("content")) if users else ""
        step_list = asked[6:].split(";;") if asked.startswith("STEPS:") else []
        after = messages[len(messages) - messages[::-1].index(users[-1]):] if users else []
        ran = sum(1 for m in after if m.get("role") == "tool")
        call = None
        if ran < len(step_list) and step_list:
            description, command = step_list[ran].split("|", 1)
            call = ("bash", {"command": command, "description": description})
        elif last.get("role") == "tool":
            delta["content"] = "done"
        else:
            prompt = text_of(last.get("content"))
            if prompt.startswith("THINK"):
                delta["reasoning_content"] = "\n".join(f"step {i}" for i in range(1, 7))
            if prompt.startswith("LONG"):
                parts = []
                for i in range(1, 41):
                    parts.append(f"## Section {i}\n\nThis paragraph is long enough to wrap across several rows in the chat's reading column, " * 3)
                    parts.append("- first point\n- second point with `code`\n- third point")
                    parts.append("```rust\nfn f(x: u32) -> u32 {\n    x * 2\n}\n```")
                    if i % 5 == 0:
                        parts.append("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |")
                delta["content"] = "\n\n".join(parts)
            if prompt.startswith("INLINE"):
                delta["content"] = "Run `cargo test` now."
            if prompt.startswith("CODE"):
                delta["content"] = "Here:\n\n```rust\n// add two numbers\nfn add(a: i32, b: i32) -> i32 {\n    a + b // \"sum\"\n}\n```\n"
            if prompt.startswith("READ:"):
                call = ("read", {"path": prompt[5:]})
            elif prompt.startswith("TOOL"):
                call = ("bash", {"command": "echo hi"})
            elif prompt.startswith("DESCRIBED:"):
                description, command = prompt[10:].split("|", 1)
                call = ("bash", {"command": command, "description": description})
            elif prompt.startswith("SHELL:"):
                call = ("bash", {"command": prompt[6:]})
            elif prompt.startswith("LUA:"):
                call = ("serpis_lua", {"code": prompt[4:]})
            elif prompt.startswith("EDIT:"):
                path, old, new = prompt[5:].split("|", 2)
                call = ("edit", {"path": path, "edits": [{"oldText": old, "newText": new}]})
            elif prompt.startswith("API:"):
                call = ("serpis_api", {"topic": prompt[4:]})
        if call:
            delta = {"role": "assistant", "tool_calls": [{
                "index": 0, "id": f"call_{count}", "type": "function",
                "function": {"name": call[0], "arguments": json.dumps(call[1])},
            }]}
            finish = "tool_calls"

        chunks = [
            {"id": "r", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": None}]},
            {"id": "r", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {}, "finish_reason": finish}]},
            {"id": "r", "object": "chat.completion.chunk", "choices": [],
             "usage": {"prompt_tokens": 1000, "completion_tokens": 10, "total_tokens": 1010}},
        ]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for c in chunks:
            self.wfile.write(f"data: {json.dumps(c)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
with open(port_file, "w") as f:
    f.write(str(server.server_address[1]))
server.serve_forever()
