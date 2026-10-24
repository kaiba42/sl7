# Bounded import pipeline experiment

This prototype overlaps file reading, chunk splitting and whole-file SHA-256
with chunk hashing, catalog lookup and pack writing. It is **not enabled in SL7**.

```sh
bash experiments/import-pipeline/run.sh
```

The script builds the pinned baseline and prototype in Docker, runs six
counterbalanced rounds, and removes its temporary context and image. It defaults
to OrbStack; set `SL7_DOCKER_CONTEXT` for another Docker context. Read the final
`RESULT` JSON for measurements.

Four modes run against a public Rust standard-library artifact repeated sixteen
times, with one byte changed after seeding the original chunks:

- `-1`: committed baseline executable.
- `0`: prototype using a sequential stream.
- `1`: producer thread sending individual chunks.
- `2`: producer thread sending batches of sixteen chunks.

The channel holds at most 32 chunks (1 MiB raw). The batch producer and consumer
can each additionally hold sixteen chunks; this queue limit is not total process
memory. Chunk boundaries, hashes, pack bytes and publication ordering are unchanged.

Every case must produce identical import reports and pack IDs and pass a complete
store integrity check. This is a CLI import microbenchmark on mostly shared data;
it does not measure Cargo, FUSE latency or cancellation/error handling. Adoption
requires those additional checks and matched build measurements.
