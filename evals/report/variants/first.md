---
name: report
description: How to write the message that ends a task, the report a person reads to review your work and decide what happens next. Outcome first, then what needs them, what changed, how it was checked and what is left. Use whenever you finish or stop a task in which you changed code, ran commands or investigated something.
---

# The report

The last message of a task is a report. The person reading it will review your work and decide
what happens next. They may come back to it after hours away, among other agents' reports. Write
so that they can act on the first two lines, and find any detail in under a minute.

## Shape

**First line: the outcome.** Start with `Done`, `Partly done` or `Blocked`, then the result in
one sentence. If something must happen before this can ship, say so here too.

> Done: the retry change is sound, but `retry_total` is now counted twice and needs a fix
> before merging.

A second sentence may give the context a reader needs to follow the rest. No preamble ("I've
finished…", "Here's a summary…"), and no restatement of what was asked.

**Then these sections, in this order, as `##` headings.** Leave out a section that has nothing
in it. Don't write "None".

1. `## Needs you`: what the reader must decide, approve or know before acting on your work.
   Number the items. Each one says what you recommend. Anything that would make a careful
   reviewer stop goes here, never further down. A warning at the bottom of a report is a
   warning nobody reads.
2. `## Changed`: one bullet per change: what, where (`path:line`) and why. Group the bullets
   by purpose, not by file.
3. `## Checked`: how you know it works. One bullet per check: what you ran, in bold, then
   what it showed, starting with `passes` or `fails`: "- **`cargo test`:** passes (214
   tests)." End with a bullet that starts `**Not checked:**` and says what you couldn't
   verify. Serpis marks each check ✓, ✗ or ○ by these words.
4. `## Not done`: what you left out, or found and didn't fix, and why.

For a task with no code changes (an investigation, a question), keep the first line and use
the sections that fit: what you found, how sure you are, what you'd do next.

## Claims

- **Say how you know.** A thing you ran or read is stated plainly, with its evidence:
  "`cargo test`: 214 passed". A thing you infer says so: "probably", "I expect", "not checked".
- **Keep confidence exact.** Never turn "may have failed" into "failed", or a result you saw
  into "should work".
- **Numbers, not adjectives.** "120 ms → 40 ms", not "significantly faster". No "robust",
  "seamless", "clean", "comprehensive".
- **One name for one thing.** If you call it `RequestQueue`, don't also call it "the queue
  manager".
- **Cite code** as `path:line`.

## Sentences

These come from ASD-STE100 (Simplified Technical English), the standard aerospace manuals are
written in so they can't be misread.

- At most 25 words a sentence, one idea each.
- Active voice that names who acts: "I moved the increment", "the client still counts it".
- Paragraphs of three sentences or fewer.
- The first words of a bullet carry its point. Start it with the thing, in bold if the list is
  long: "**Fixed a race in `RequestQueue::retry`**…".
- No semicolons. Split the sentence.
- Plain verbs: "start", not "spin up"; "use", not "leverage"; "check", not "perform a check".
- Keep the present perfect when it says something is true now: "the job has finished".
- Define a term the reader may not know, once, the first time you use it.

## Length

As long as the work needs, and no longer. A one-line fix gets a one-line report. Use prose for
reasoning (why this approach over that one) and bullets for lists of things. No closing offer
("Let me know if…"): the reader knows they can ask.
