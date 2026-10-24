# Reusing the chunker read buffer

The 128 KiB buffer keeps unread bytes in place between chunks and compacts only when refilling. Chunk selection remains limited to 32 KiB; identities and pack format are unchanged. This reduces copying without adding threads.

| Import | Baseline (s) | Buffer change (s) | Median reduction | Faster paired rounds |
| --- | ---: | ---: | ---: | ---: |
| original | 0.3489 | 0.3407 | 2.35% | 6/6 |
| edited | 0.3238 | 0.3106 | 4.07% | 6/6 |

These are sequential CLI imports of a public Rust standard-library archive repeated sixteen times, first unchanged and then with one byte edited. Six reversed-order rounds, warm caches, Oaktown top. All twelve stores passed integrity checking and both executables produced matching import reports and pack hashes. This does not establish a Cargo build speedup.

The candidate passed 81 Linux tests, strict Clippy, all five mounted suites, and the large-file mmap/daemon-death recovery test with the pipeline enabled. Host validation passed 44 tests. Fixed scalar chunk boundaries and short/interrupted reads are covered.

[Full import measurements](chunker-buffer-import.json) · [Validation gate](chunker-buffer-validation.json)
