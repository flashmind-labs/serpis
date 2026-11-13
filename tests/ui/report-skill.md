Done: the retry change is sound, but `retry_total` is now counted twice and needs a fix before merging. The PR moves retries from the HTTP client into `RequestQueue`, so a retried request keeps its place in the queue.

## Needs you

1. **`retry_total` is counted twice.** The client still increments it, and the queue now does too, so the numbers double. I recommend removing the client-side counter before merging. I didn't change the metrics code.
2. **Dashboards may misread `Error::RetriesExhausted`.** Its `Display` output now includes the attempt count, and some dashboards parse that message. They may need updating.
3. **A request that keeps failing can hold up the others.** Retried requests go to the front of the queue until they reach the retry limit (5). With the current backoff I expect this is fine. If you want a guard, I can cap how many retried requests wait at the front.

## Changed

- **Fixed a race in `RequestQueue::retry`** (`src/queue.rs:142`). The retry counter went up after the request went back in the queue, so a fast worker could read the old count. The increment now comes first. This is the likely cause of the flaky `retries_respect_limit` test in CI.
- **Added ±20% jitter to the backoff** (`src/backoff.rs:28`). Retries from many clients were synchronizing after an outage. The old client did the same.
- **Removed `RetryPolicy::legacy`.** Nothing calls it after this change.

```rust
let attempt = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
if attempt > self.limit {
    return Err(Error::RetriesExhausted(attempt));
}
self.queue.push_front(req);
```

## Checked

- **Full test suite:** passes.
- **`retries_respect_limit`, 200 runs in a loop:** passed every time. The race was intermittent, so this makes the fix likely, not certain.
- **Integration tests against the staging mock:** pass.
- **Not checked:** the dashboards, and whether a failing request holds up others under real load.
