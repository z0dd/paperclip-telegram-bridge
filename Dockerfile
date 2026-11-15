# Builder stage
FROM rust:1.100.0-slim-bookworm AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

RUN cargo build --release --locked

# Runtime stage
FROM debian:bookworm-slim

WORKDIR /app

RUN useradd -m -u 65532 nonroot && \
    mkdir -p /state && \
    chown nonroot:nonroot /state

# Copy binary from builder
COPY --from=builder /app/target/release/bridge /usr/local/bin/bridge
RUN chmod +x /usr/local/bin/bridge

VOLUME ["/state"]
USER nonroot:65532

ENTRYPOINT ["/usr/local/bin/bridge"]
CMD ["run"]

STOPSIGNAL SIGTERM
