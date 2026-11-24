---
name: report
description: How to write the message that ends a task, the report a person reads to review your work and decide what happens next. Short and plain, outcome first, then what needs them, what changed, how it was checked and what is left. Use whenever you finish or stop a task in which you changed code, ran commands or investigated something, a reply to review comments included.
---

# The report

The last message of a task is a report. The reader is busy. They may come back to it after
hours away, among other agents' reports. Tell them what they need to act, then stop.

## Shape

**First line: the outcome.** `Done`, `Partly done` or `Blocked`, then the result in one
sentence. If something must happen before this ships, say it here.

> Done: retries work, but `retry_total` is now counted twice. Fix that before merging.

**Then, if there is a real next step, one line: `**Next:**`** what you'd do next and the
reason. A reason is the fact the choice rests on, in one line, not "it's cleaner".

**Then these `##` sections, in this order, and no others.** Leave out a section with nothing
in it. Don't rename one or add your own: serpis and your reader find a report's parts by these
names.

1. `## Needs you`: what is still open for the reader: a choice to make, something to approve,
   a risk to know before it ships. Number the items, each with what you recommend, the reason,
   and the main alternative and why not. Anything that would stop a careful reviewer goes here,
   never lower down. Work you finished goes under Changed, even when it settles an earlier item.
2. `## Changed`: one bullet per change: what, where (`path:line`), why. Group by purpose, not
   by file. Changes someone asked for come in the order they asked.
3. `## Checked`: one bullet per check: the command in bold, then `passes` or `fails` and what
   it showed: "- **`cargo test`:** passes (214 tests)." Last, a bullet starting
   `**Not checked:**` with what you couldn't verify. Serpis marks each check ✓, ✗ or ○ by
   these words.
4. `## Not done`: what you left out, or found and didn't fix, and why.

No code changed (an investigation, a question)? Keep the first line, then what you found, how
sure you are, what you recommend and the reason.

## Told what to report

Another skill, the person or their review comments may say what to report: a code review's
findings, "say what you did for each". That decides what goes in. This skill still decides the
shape: the same first line and sections, none added.

For a code review:

- **First line:** the verdict and the count: "Done: 2 must-fix, 3 to consider."
- **`## Needs you`:** one numbered item per finding, must-fix first. The tag, `path:line`, the
  issue and your suggestion, in two lines at most.
- **`## Checked`:** the commands and probes you ran, one line each.

## After review comments

Comments on your report come as one message: "Review comments on what you wrote". Your reply is
a new report on what you did since the last one.

- **A comment:** a bullet under `## Changed`, said as the change. One you didn't act on,
  or made another way, goes under `## Needs you` with the reason.
- **A question:** answer it after the first line, quoting the words it asks about.
- **A challenge from another agent:** fix it (a bullet under `## Changed`), or answer it after
  the first line, quoting it, with what shows it's wrong.
- **"Item 2 under Needs you"** is a place in your last report, not a heading or a name for
  this one. An item you settled leaves `## Needs you`.

```markdown
Done: retries back off now, and the race you asked about is fixed.

> This is the likely cause of the flake.

Yes, I reproduced it: 3 of 500 runs failed with 8 threads, none after the fix.

## Changed
- **Backoff between retries** (`queue.rs:88`): 1 s, 2 s, then 4 s, as you asked.
- **Attempt count taken under the lock** (`queue.rs:61`): two retries shared one before.

## Checked
- **`cargo test`:** passes (216 tests).
- **`retries_respect_limit` 500 times, 8 threads:** passes (0 failures).
```

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
