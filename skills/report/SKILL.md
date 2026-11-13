---
name: report
description: How to write the message that ends a task, the report a person reads to review your work and decide what happens next. Short and plain, outcome first, then what needs them, what changed, how it was checked and what is left. Use whenever you finish or stop a task in which you changed code, ran commands or investigated something.
---

# The report

The last message of a task is a report. The reader is busy. They may come back to it after
hours away, among other agents' reports. Tell them what they need to act, then stop.

## Shape

**First line: the outcome.** `Done`, `Partly done` or `Blocked`, then the result in one
sentence. If something must happen before this ships, say it here.

> Done: retries work, but `retry_total` is now counted twice. Fix that before merging.

**Then these `##` sections, in this order.** Leave out a section with nothing in it.

1. `## Needs you`: what the reader must decide, approve or know. Number the items, each with
   what you recommend. Anything that would stop a careful reviewer goes here, never lower down.
2. `## Changed`: one bullet per change: what, where (`path:line`), why. Group by purpose, not
   by file.
3. `## Checked`: one bullet per check: the command in bold, then `passes` or `fails` and what
   it showed: "- **`cargo test`:** passes (214 tests)." Last, a bullet starting
   `**Not checked:**` with what you couldn't verify. Serpis marks each check ✓, ✗ or ○ by
   these words.
4. `## Not done`: what you left out, or found and didn't fix, and why.

No code changed (an investigation, a question)? Keep the first line, then what you found, how
sure you are and what you'd do next.

## With other skills

Another skill may say what to report, such as a code review's findings. That skill decides
what goes in. This one decides the shape. For a review:

- **First line:** the verdict and the count: "Done: 2 must-fix, 3 to consider."
- **`## Needs you`:** one numbered item per finding, must-fix first. The tag, `path:line`, the
  issue and your suggestion, in two lines at most.
- **`## Checked`:** the commands and probes you ran, one line each.

## Short

- **A one-line fix gets a one-line report.** Most reports fit on one screen.
- **Short, crisp paragraphs:** three sentences at most. Bullets of one line, two at most.
  Sentences under 25 words.
- **Lead with the point.** The first words of a bullet say what it is about:
  "**Fixed a race in `RequestQueue::retry`:** …".
- **Cut what doesn't change what the reader knows or does:** files you read, steps you took,
  things that went as expected, the same point said twice.

## Plain

Write it the way you'd tell a colleague at their desk. A short phrase beats a formal one, even
if it's conversational.

> Not: "I've implemented a robust retry mechanism, ensuring requests are reliably re-sent."
> But: "Failed requests now retry 3 times, 1 s apart (`queue.rs:88`)."

Cut:

- **Openers and closers:** "I've finished…", "Here's a summary…", "Let me know if…", a last
  line that sums up what you just said, a restatement of the task.
- **Padding:** "it's worth noting that", "it's important to", "in order to" (to), "due to the
  fact that" (because), "at this point" (now), "a number of" (the number).
- **Inflated words:** robust, seamless, comprehensive, crucial, key, significant, leverage,
  utilize, ensure, enhance, streamline, delve. Say what it does.
- **Tacked-on clauses:** "…, ensuring that X", "…, making it easier to Y". If it matters, give
  it its own sentence.
- **Set phrases:** "not just X but Y", lists of three for rhythm, "serves as" (is).
- **Stacked hedges:** "could potentially". Hedge once, and only when you're unsure.

Plain verbs ("start", not "spin up"), active voice that names who acts ("I moved the
increment"), no semicolons.

## Claims

- **Say how you know.** What you ran or read, state with its evidence: "`cargo test`: 214
  passed". What you infer, mark: "probably", "not checked".
- **Keep confidence exact.** "May have failed" never becomes "failed". A result you saw never
  becomes "should work".
- **Numbers, not adjectives:** "120 ms → 40 ms", not "much faster".
- **One name for one thing.** If you call it `RequestQueue`, don't also call it "the queue
  manager".

Before you send it, reread it once. Cut every sentence the reader wouldn't miss. Turn every
phrase that could be one word into that word.
