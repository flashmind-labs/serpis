"""A local OpenAI-compatible model for UI tests: no network, no keys.

Every response reports 1000 prompt tokens and 10 completion tokens (tests price them in
models.json). What it says depends on the last user message:

  THINK ...       think six lines, then reply "ok"
  TOOL ...        call bash `echo hi`
  CODE ...        reply with a fenced Rust code block
  INLINE ...      reply with inline code (Run `cargo test` now.)
  LONG ...        reply with a long answer (paragraphs, lists, code, a table)
  REPORT          reply with report.md (beside this file): an agent's summary of a code review
  REPORT:<name>   reply with report-<name>.md: the same review written another way (skill: by
                  skills/report/SKILL.md)
  /skill:challenge, expanded (a report's challenge): reply with challenge.md, what a
                  challenger found wrong with report-skill.md; with NO REPORT in it, think and
                  stop out of output tokens, no text; with SLOW in it, call bash `sleep 20`
  CHALLENGE       the same reply, sent in a challenger's chat (it goes on after it stopped).
                  Offered the challenge tools (and without NO TOOLS, NO REPORT or SLOW), it
                  first records challenge.md's findings and held claims with them, one call a
                  request: the third finding's quote is not in the report, and it calls again
                  with one that is once told so
  Prove this finding ...   (Prove it on a challenge's finding) confirm it, with evidence
                  (with challenge_proof when offered)
  Review comments on what you wrote. ...   (a review serpis sent) answer each comment,
                  quoting it as asked: "> quote" then "Answer N."
  READ:<path>     call read on that path
  SHELL:<command> call bash with that command
  DESCRIBED:<description>|<command> the same, saying what it is for
  STEPS:<description>|<command>;;... several, one after another
  LUA:<code>      call serpis_lua with that code
  API:<topic>     call serpis_api with that topic
  SEARCH:<query>  call web_search with that query. ui-test.sh points DuckDuckGo here: it gets
                  one result, or, with "unreachable" in the query, a 501 (every search fails)
  EDIT:<path>|<old>|<new>   call edit on that file, replacing old with new
  anything else   reply "ok"

After a tool result it replies "done". Each request is logged as one JSON line to the file
given as the second argument: {"n", "last_role", "tool_content", "tools", "images", "text",
"skills", "serpis_docs", "report_skill", "report_rule", "report_text", "diagrams_skill"} (images: how many the last message carries; text: its text; skills:
whether the system prompt lists skills; serpis_docs: whether it points requests about serpis at
serpis_api; diagrams_skill: whether it offers the diagrams skill; report_skill, report_rule: whether it offers the report skill and says to end
each task with a report; report_text: whether the skill's own text comes with it).

  python3 mock_llm.py <port-file> <log-file>    (listens on a free port, written to port-file)
"""

import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

port_file, log_file = sys.argv[1], sys.argv[2]

# What a challenger offered its tools records (challenge.md's findings), one call a request.
CHALLENGE_CALLS = [
    ("challenge_finding", {"claim": "Changed 1", "status": "confirmed", "quote": "This is the likely cause of the flaky `retries_respect_limit` test in CI.",
                           "finding": "The race is still there: `push_front` runs before the counter is stored. It reads the old count.",
                           "evidence": "`retries_respect_limit` with 8 workers failed 3 of 500 runs."}),
    ("challenge_finding", {"claim": "Changed 3", "status": "plausible", "quote": "Nothing calls it after this change.",
                           "finding": "A test helper in `tests/common.rs` still calls it.", "evidence": "Read tests/common.rs:12."}),
    # Not the report's words: the tool says so, and it tries again with words that are.
    ("challenge_finding", {"claim": "Checked 2", "status": "plausible", "quote": "passed two hundred times",
                           "finding": "The loop ran on one thread, which can't hit a race between workers.", "evidence": "Read the test."}),
    ("challenge_finding", {"claim": "Checked 2", "status": "plausible", "quote": "passed every time.",
                           "finding": "The loop ran on one thread, which can't hit a race between workers.", "evidence": "Read the test."}),
    ("challenge_held", {"claim": "Changed 2", "note": "The backoff adds ±20%."}),
    ("challenge_held", {"claim": "Checked 1", "note": "Reran it."}),
]
count = 0
busy = set()


def text_of(content):
    if isinstance(content, str):
        return content
    return "".join(p.get("text", "") for p in content or [] if isinstance(p, dict))


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        # DuckDuckGo's HTML search, as serpis's web_search reads it.
        from urllib.parse import parse_qs, urlparse
        q = parse_qs(urlparse(self.path).query).get("q", [""])[0]
        if "unreachable" in q:
            self.send_response(501)
            self.end_headers()
            return
        page = ('<div class="result results_links results_links_deep web-result">'
                '<a rel="nofollow" class="result__a" href="https://example.com/rust">Rust results</a>'
                '<a class="result__snippet" href="https://example.com/rust">A page about rust.</a>'
                '<div class="clear"></div></div>')
        self.send_response(200)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(page.encode())

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
                "text": text_of(last.get("content"))[:12000],
                "skills": "<available_skills>" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "serpis_docs": "Call serpis_api first" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "report_skill": "<name>report</name>" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "report_rule": "your last message is a report" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "report_text": '<skill name="report">' in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
                "diagrams_skill": "<name>diagrams</name>" in text_of(messages[0].get("content")) if messages and messages[0].get("role") == "system" else False,
            }) + "\n")

        prompt = text_of(last.get("content"))
        if last.get("role") == "user" and prompt.startswith("BUSY") and prompt not in busy:
            busy.add(prompt)
            payload = json.dumps({"error": {"message": "Rate limit exceeded", "code": 429}}).encode()
            self.send_response(429)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(payload)))
            self.end_headers()
            self.wfile.write(payload)
            return

        delta, finish = {"role": "assistant", "content": "ok"}, "stop"
        # STEPS: one command after another in the same turn.
        users = [m for m in messages if m.get("role") == "user"]
        asked = text_of(users[-1].get("content")) if users else ""
        step_list = asked[6:].split(";;") if asked.startswith("STEPS:") else []
        after = messages[len(messages) - messages[::-1].index(users[-1]):] if users else []
        ran = sum(1 for m in after if m.get("role") == "tool")
        call = None
        tools = [t["function"]["name"] for t in body.get("tools", [])]
        challenging = (asked.startswith('<skill name="challenge"') or asked.startswith("CHALLENGE")) and "challenge_finding" in tools \
            and not any(k in asked for k in ("NO TOOLS", "NO REPORT", "SLOW"))
        if ran < len(CHALLENGE_CALLS) and challenging:
            call = CHALLENGE_CALLS[ran]
        elif asked.startswith("Prove this finding") and "challenge_proof" in tools and ran == 0:
            call = ("challenge_proof", {"outcome": "confirmed", "evidence": "3 of 500 runs failed with 8 threads."})
        elif ran < len(step_list) and step_list:
            description, command = step_list[ran].split("|", 1)
            call = ("bash", {"command": command, "description": description})
        elif last.get("role") == "tool" and challenging:
            with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "challenge.md")) as f:
                delta["content"] = f.read()
        elif last.get("role") == "tool" and asked.startswith("Prove this finding"):
            delta["content"] = "Confirmed: with 8 threads it fails."
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
            if prompt.startswith("Review comments on what you wrote."):
                answers, group = [], []
                for line in prompt.split("\n") + [""]:
                    if line.startswith("> "):
                        group.append(line)
                    elif group:
                        answers.append("\n".join(group) + f"\n\nAnswer {len(answers) + 1}.")
                        group = []
                delta["content"] = "\n\n".join(answers) or "ok"
            if prompt.startswith("REPORT"):
                variant = prompt[len("REPORT:"):].strip() if prompt.startswith("REPORT:") else ""
                name = f"report-{variant}.md" if variant.isidentifier() else "report.md"
                with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), name)) as f:
                    delta["content"] = f.read()
            if prompt.startswith('<skill name="challenge"') or prompt.startswith("CHALLENGE"):
                with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "challenge.md")) as f:
                    delta["content"] = f.read()
                # Told so in its note: run out of output tokens while thinking, or take a while.
                if "NO REPORT" in prompt:
                    delta = {"role": "assistant", "reasoning_content": "Let me think about the race for a long while"}
                    finish = "length"
                if "SLOW" in prompt:
                    call = ("bash", {"command": "sleep 20"})
            if prompt.startswith("Prove this finding"):
                delta["content"] = "[confirmed] With 8 threads it fails.\nEvidence: 3 of 500 runs failed with 8 threads."
            if prompt.startswith("LINKS"):
                delta["content"] = "[Docs](https://example.com/docs) are here, or see https://example.org/page."
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
            elif prompt.startswith("SEARCH:"):
                call = ("web_search", {"query": prompt[7:]})
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
