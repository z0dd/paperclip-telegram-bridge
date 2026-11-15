# Builder stage
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

RUN cargo build --release --locked

# Runtime stage - using simple alpine as placeholder
FROM debian:bookworm-slim@sha256:d5c0218172bec78acdce7a55a3b2f78d49dfdac94d1c4bf48a8234cedc8a1ab4

WORKDIR /app

RUN useradd -m -u 65532 nonroot && \
    mkdir -p /state && \
    chown nonroot:nonroot /state

# Copy binary from builder
COPY --from=builder /app/target/release/bridge /usr/local/bin/bridge
RUN chmod +x /usr/local/bin/bridge

VOLUME ["/state"]
USER nonroot:nonroot

ENTRYPOINT ["/usr/local/bin/bridge"]
CMD ["run"]

STOPSIGNAL SIGTERM
