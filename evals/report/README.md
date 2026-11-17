# The report eval

Does a change to the report skill (`skills/report/SKILL.md`) make agents write better reports?
This measures it. serpis's own agent does real tasks on fresh clones of this repository, once per
variant of the skill, and each report it ends with is scored as serpis reads it.

```sh
scripts/report-eval.sh --n 10 --variants current,proposed
scripts/report-eval.sh --score target/report-eval/<run>    # score a run again, no model calls
```

It spends real money on OpenRouter, with the key serpis has (`OPENROUTER_API_KEY` wins). The
default model is `deepseek/deepseek-v4.1-flash-1017` with thinking at max (`--model`,
`--thinking`). A run's cost is in its summary.

## What runs

- **Tasks** (`tasks.json`): real work on this repository at a pinned commit (`--commit`, HEAD by
  default). Two investigations of real bugs, which ask for no code change, and one code change.
  After the change, serpis's review message (`chat_review::prompt`, its default settings) is
  sent with a change asked for on the report's first Needs you item (else its first Changed
  item) and a question on its first Changed item. The reply is scored as its own step.
- **Variants:** `current` is `skills/report/SKILL.md` as it is in the working tree. Any other
  name is `variants/<name>.md`.
- **Each run** gets its own clone, agent directory and `HOME`, with no API keys in its
  environment. Its system prompt is built as a serpis chat builds it: the built-in tools (no
  web search), the skills list, and the report rule with the variant's text. serpis's Lua tools
  and docs and your config's additions are left out. Cargo builds share
  `target/report-eval/cargo-target`, so dependencies build once. This isn't a sandbox: the
  agent's commands can still read the rest of the machine (one run read leftover UI-test logs in
  `/tmp`). The tasks give them no reason to change anything outside the clone.

Everything a run did is kept under `target/report-eval/<run>/runs/<task>/<variant>/<i>/`: the
system prompt, the transcript, `report.md`, `review.md` and `reply.md`, and the scores.

## What is scored

| Measure | Meaning |
|---|---|
| parsed | serpis reads it as a report (`buffers::chat::shaped_report`): it opens with Done, Partly done or Blocked |
| clean | parsed, only the skill's sections in its order, Needs you first, no bold or `###` headings of its own |
| invented sections | `##` sections other than Needs you, Changed, Checked, Not done |
| bold/### headings | a line that is all bold, or a `#`/`###` heading: structure the skill doesn't have |
| words, words before sections | medians, code blocks left out |
| biggest Needs you item | median of each report's longest item, in words |
| Not checked | Checked ends with a `**Not checked:**` bullet |
| answered (reply) | the reply quotes the question's first line, as serpis needs to link the answer |
| label reused (reply) | the reply copies a place the review named ("Needs you 1", "item 1 under") |

The summary compares the first two variants over all steps with a two-sided Fisher exact test
on parsed and clean.

Agents are random, so one sample proves nothing. Use `--n 10` or more per variant before
drawing a conclusion. OpenRouter may route a model to different upstreams; each record lists
the ones it used.
