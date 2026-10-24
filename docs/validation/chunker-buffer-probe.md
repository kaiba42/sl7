# Chunker buffer experiment

Keeping a 128 KiB buffer and advancing a cursor reduced copying while preserving every chunk length and identity in all six fixtures. Separating the two boundary-mask loops made the prototype slower and was rejected.

| Fixture | Original (ms) | 128 KiB buffer (ms) | Reduction |
| --- | ---: | ---: | ---: |
| stdlib | 95.2074 | 87.4694 | 8.1% |
| zeros | 17.0763 | 16.4073 | 3.9% |
| random | 17.2375 | 14.8258 | 14.0% |
| small-4k | 0.0066 | 0.0061 | 6.7% |
| small-64k | 0.0332 | 0.0301 | 9.2% |
| small-1m | 0.4709 | 0.4051 | 14.0% |

Oaktown top, Ryzen 9 7940HS, Rust 1.97.1. Six reversed-order rounds; each sample averages 8–4,096 scans. Hashing verifies exact parity before timing and is excluded from timed scans. Filesystem caches are warm. Both prototype objects are initialized in each scan; only the selected one is consumed. These are chunking measurements, not Cargo build timings.

The checked-in reproduction script also passed all 144 samples and identity comparisons on macOS; those timings are not mixed into the x86 summary.

[Reproduce](../../experiments/chunker-buffer/README.md) · [All measurements](chunker-buffer-probe.json) · [Production import comparison](chunker-buffer-import.md)
