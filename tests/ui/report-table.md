Done: the cache now evicts by size, and hit rate is up on all three workloads.

| Workload | Before | After | Change |
|---|--:|--:|--:|
| Chat | 61% | 78% | +17 |
| Review | 44% | 70% | +26 |
| Long session with many tool calls and large outputs | 52% | 66% | +14 |

## Needs you

1. **Pick the size cap.** I recommend 256 MB. Here is what each cap costs:

   | Cap | Hit rate | Memory |
   |---|--:|---|
   | 128 MB | 71% | low |
   | 256 MB | 78% | medium |

2. **Approve dropping the old `lru` crate** from `Cargo.toml:41`.

| Option | Hit rate | Notes |
|---|---|---|
| Size cap | 78% | what this change does |
| Count cap | 61% | the old behaviour, kept behind a setting |

## Changed

- **Eviction by size** in `src/cache.rs:88`, so one large tool output no longer pushes out fifty small ones.

| File | Lines | Why |
|---|--:|---|
| `src/cache.rs` | +64 −21 | eviction by size, measured on insert, with a running total kept beside the map |
| `src/config.rs` | +8 | the cap setting |

## Checked

- **`cargo test`:** passes (214 tests).
- **`cargo bench cache`:** passes, numbers in the table above.
- **Not checked:** memory use over a day-long session.
