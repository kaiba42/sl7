# Passthrough lifetime experiment

This standalone one-file FUSE server tests whether an exclusive native-file lease
can detect mappings that outlive FUSE release. It does not implement SL7 storage,
sealing, metadata overrides, or crash recovery.

Requires Linux with FUSE passthrough, `/dev/fuse`, `fusermount3`, Python 3.11+,
and mount privileges (`CAP_SYS_ADMIN`). The tested backing filesystem is ext4.
The driver creates and removes its own temporary file and mount under `--work-root`.

```sh
cargo build --locked --release \
  --manifest-path experiments/passthrough-lifetime/Cargo.toml \
  --target-dir /tmp/sl7-passthrough-target
sudo python3 experiments/passthrough-lifetime/probe.py \
  --binary /tmp/sl7-passthrough-target/release/sl7-passthrough-proof \
  --work-root /path/on/ext4
```

It checks read-only, private writable, shared writable, and child-held shared
mappings. Native mapping calls use libc directly so Python does not retain an
extra file descriptor. The driver verifies lease denial while mapped, lease
acquisition after unmapping, reopen behavior, and private/shared write visibility.

The smaller native-only control can also be run on a chosen filesystem:

```sh
cc -O2 -Wall -Wextra -Werror experiments/passthrough-lifetime/native-lease.c \
  -o /tmp/sl7-native-lease
/tmp/sl7-native-lease /path/to/work-directory
```

The control reports observations; it does not reject an unsafe lease grant.
On the tested overlayfs, a live native mapping did **not** block the lease.
[Results and integration constraints](../../docs/validation/passthrough-lifetime.md).
