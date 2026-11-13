I've finished the review of the retry changes and made a couple of fixes along the way. Here's a summary of what I found and what I changed.

## Summary

The PR moves request retries from the HTTP client into `RequestQueue`, so that a retried request keeps its place in the queue instead of going to the back. Overall the approach looks good and the code is well structured, but there were a few issues that I addressed, and a couple of things you might want to look at before merging.

## Changes made

- **Fixed a race in `RequestQueue::retry`** (`src/queue.rs:142`): the retry counter was incremented after the request was re-enqueued, so a fast worker could pick it up and see the old count. I moved the increment before the push. This should fix the flaky `retries_respect_limit` test that was failing intermittently in CI.
- **Backoff now has jitter** (`src/backoff.rs:28`): retries from many clients were synchronizing after an outage. I added ±20% jitter, which is what the old client did.
- **Removed the unused `RetryPolicy::legacy` constructor**, which nothing calls anymore after this change.

```rust
let attempt = self.attempts.fetch_add(1, Ordering::AcqRel) + 1;
if attempt > self.limit {
    return Err(Error::RetriesExhausted(attempt));
}
self.queue.push_front(req);
```

## Testing

I ran the full test suite and everything passes. I also ran `retries_respect_limit` 200 times in a loop and it passed every time, so the race seems to be fixed. The integration tests against the staging mock also pass.

## Things to consider

- The new queue keeps retried requests at the front, which means a request that keeps failing can starve the others until it hits the retry limit. With the default limit of 5 and the current backoff this is probably fine, but it might be worth capping how many retried requests can sit at the front at once.
- `Error::RetriesExhausted` now carries the attempt count, which changes its `Display` output. Some of the dashboards parse that message, so they may need updating.
- I didn't touch the metrics code, but `retry_total` is now counted in two places (the client still increments it), so the numbers will be doubled until the client-side counter is removed. This should probably be fixed before merging.

Let me know if you'd like me to make any of these changes or look into anything else.
