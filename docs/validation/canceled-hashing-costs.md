# Hashing discarded by canceled sealing

During three edits, 21.4% of chunk-hashing bytes belonged to canceled preparations. Read-only reopens accounted for 97.5% of those discarded bytes. This suggests retaining completed preparations across read-only interference, subject to memory bounds and epoch validation.

| Stage | Hashed MiB | Discarded MiB | Reopen-discarded MiB | Epoch-change-discarded MiB | Canceled preparations | Canceled preparation service (s) |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| clean | 8119.07 | 350.01 | 232.31 | 117.70 | 767 | 4.505 |
| unchanged-1 | 0.00 | 0.00 | 0.00 | 0.00 | 0 | 0.000 |
| unchanged-2 | 0.00 | 0.00 | 0.00 | 0.00 | 0 | 0.000 |
| unchanged-3 | 0.00 | 0.00 | 0.00 | 0.00 | 0 | 0.000 |
| edit-1 | 1463.32 | 242.24 | 242.24 | 0.00 | 3 | 0.714 |
| edit-2 | 1712.60 | 499.92 | 484.48 | 15.44 | 6 | 0.846 |
| edit-3 | 1464.65 | 251.97 | 242.24 | 9.73 | 5 | 0.424 |
| unchanged-after-remount | 0.00 | 0.00 | 0.00 | 0.00 | 0 | 0.000 |

All eight Cargo commands, executable/freshness checks, eight artifact-graph comparisons against the prior native sequence, incremental-output checks, and the store integrity check passed. Each observation had closed handles and drained work.

Not a performance comparison or a statistical estimate.

Executable-help checks can reopen the just-built executable while it is being sealed; this is a build-and-run workflow observation.

Epoch mutations include namespace and metadata changes.

Bytes measure chunk hashing, excluding whole-file/pack hashing and producer prefetch.

Preparation durations are cumulative elapsed service time, not CPU time or Cargo overhead.

[Diagnostic JSON](canceled-hashing-costs.json) · [Counter validation](canceled-hashing-validation.json)
