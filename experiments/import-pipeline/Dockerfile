FROM rust:1.97-bookworm@sha256:0e2bcaef56d041a486784e54104a81aebe0da44bd03019bd70bc0401e42e4a97
RUN apt-get update && apt-get install -y --no-install-recommends python3 && rm -rf /var/lib/apt/lists/*
WORKDIR /sl7
COPY Cargo.toml Cargo.lock ./
COPY baseline-src/ src/
COPY benches/ benches/
COPY tests/ tests/
RUN cargo build --locked --release && cp target/release/sl7 /sl7-baseline
COPY src/ src/
RUN cargo clean -p sl7 --release && cargo test --locked && cargo build --locked --release
COPY scripts/pipeline-probe.py /pipeline-probe.py
CMD ["python3", "/pipeline-probe.py"]
