# Chunker buffer experiment

```sh
bash experiments/chunker-buffer/run.sh
```

Requires Rust and Python 3; measured with Rust 1.97.1. Uses a disposable directory
and the pinned original chunker from Git. The final JSON records the toolchain,
source and fixture hashes, and all samples. No private build artifacts are used.

Four prototypes run in six reversed-order rounds against a public Rust
standard-library archive, random data, zeros, and three small-file fixtures:

- `0`: original 32 KiB buffer, shifted and refilled after each chunk.
- `1`: fixed 32 KiB buffer with cursors, without repeated zero-filling.
- `2`: 128 KiB buffer, compacted only when less than 32 KiB remains.
- `3`: mode 2 with separate scanning loops for the two boundary masks.

All modes must match the original chunk lengths and SHA-256 identities before
timing. Timed scans check byte/chunk counts and consume each allocated chunk;
they exclude hashing, compression, catalog work and Cargo. Every sample averages
8–4,096 scans depending on input size. Filesystem caches are warm.

Each scan initializes both prototype objects, then consumes only the selected
one; this is a controlled implementation experiment, not production import
latency. Full imports and matched builds are separate validation gates.
