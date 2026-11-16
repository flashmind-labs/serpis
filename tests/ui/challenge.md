Done: three of the report's claims may not hold, two held.

## Needs you

1. **[confirmed] Changed 1**
   > This is the likely cause of the flaky `retries_respect_limit` test in CI.
   The race is still there: `push_front` runs before the counter is stored.
   Evidence: `retries_respect_limit` with 8 workers failed 3 of 500 runs.
2. **[plausible] Changed 3**
   > Nothing calls it after this change.
   A test helper in `tests/common.rs` still calls it.
3. **[plausible] Checked 2**
   > passed two hundred times
   The loop ran on one thread, which can't hit a race between workers.

## Checked

- **Changed 2:** held. The backoff adds ±20%.
- **Checked 1:** held, reran it.
