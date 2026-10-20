"""A local OpenAI-compatible model for UI tests: no network, no keys.

Every response reports 1000 prompt tokens and 10 completion tokens (tests price them in
models.json). What it says depends on the last user message:

  TOOL ...        call bash `echo hi`
  LUA:<code>      call serpis_lua with that code
  API:<topic>     call serpis_api with that topic
  anything else   reply "ok"

After a tool result it replies "done". Each request is logged as one JSON line to the file
given as the second argument: {"n", "last_role", "tool_content", "tools"}.

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
            }) + "\n")

        delta, finish = {"role": "assistant", "content": "ok"}, "stop"
        if last.get("role") == "tool":
            delta["content"] = "done"
        else:
            prompt = text_of(last.get("content"))
            call = None
            if prompt.startswith("TOOL"):
                call = ("bash", {"command": "echo hi"})
            elif prompt.startswith("LUA:"):
                call = ("serpis_lua", {"code": prompt[4:]})
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
