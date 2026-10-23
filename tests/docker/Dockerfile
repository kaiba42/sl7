FROM rust:1.97-bookworm
RUN rustup component add clippy
RUN apt-get update && apt-get install -y --no-install-recommends fuse3 python3 \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /sl7
COPY Cargo.toml Cargo.lock ./
COPY src/ src/
COPY tests/ tests/
COPY benches/ benches/
COPY scripts/ scripts/
RUN cargo test --locked --features fuse && cargo build --locked --features fuse \
    && cargo clippy --locked --all-targets --features fuse -- -D warnings \
    && cargo build --locked --release --features fuse
ENV SL7_BINARY=/sl7/target/release/sl7
CMD ["python3", "scripts/container-test.py"]
