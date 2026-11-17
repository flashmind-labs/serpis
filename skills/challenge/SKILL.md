---
name: challenge
description: Challenge another agent's report. Try to prove its claims wrong by reading its changes and running its checks again, then report each claim you broke in a shape serpis places on that report. Used by serpis's Challenge button on a report, by name only.
disable-model-invocation: true
---

# Challenge a report

Another agent finished a task and wrote a report. Your job is to find where it is wrong. You
are not its reviewer for style or taste. You are looking for claims that are false, or that
its evidence doesn't support.

You work in a copy of its worktree: the same commits, with its uncommitted changes copied in.
Change nothing that matters. Your edits are thrown away with the copy.

## What to check

The report's claims are its headline, each item under `## Changed` and each under `## Checked`.
Name each claim by its section and number: `Changed 2` is the second item under Changed,
`Headline` is the first sentence.

For each claim, ask: is it true, and does what the agent did show it?

- **Changed:** read the code at the place it names. Does the change do what the report says?
  Does it break something next to it?
- **Checked:** a check that "passes" may not test what it claims. Run it again when you may
  run commands. Look at how it ran: a loop on one thread can't show a race is gone.
- **Headline:** does the rest of the report back it up?

Prefer running something to arguing. A finding you reproduced is worth ten you reasoned out.

## What not to report

- Style, naming, or how you'd have written it.
- Claims you checked and could not break. Say they held, under Checked.
- Guesses with no failure case. Each finding needs a concrete way it goes wrong.

## Your report

End with a report, as the `report` skill says. Put each finding under `## Needs you`, in
exactly this shape, so serpis can place it on the report it challenges:

```
## Needs you

1. **[confirmed] Changed 1**
   > This is the likely cause of the flaky `retries_respect_limit` test in CI.
   The race is still there: `push_front` at `src/queue.rs:147` runs before the counter is
   stored, so a second worker can read the old count.
   Evidence: `retries_respect_limit` with 8 workers failed 3 of 500 runs.
```

- **The tag:** `[confirmed]` when you ran something that shows it, `[plausible]` when you only
  read the code.
- **The place:** the claim's section and number, as above, or `Headline`.
- **The quote:** the exact words of the claim you dispute, copied from the report, on a line
  starting with `> `. Quote one sentence, not the whole item.
- **The finding:** what is wrong, in two sentences at most.
- **Evidence:** what you ran and what it showed, or what you read. One line.

Under `## Checked`, list the claims that held, one per line: `- **Changed 2:** held. Read the
backoff, it adds ±20%.` No findings at all: say so in your first line.
