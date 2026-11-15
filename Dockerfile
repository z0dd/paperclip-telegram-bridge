# Builder stage
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder

WORKDIR /app

# Copy Cargo files first for layer caching
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch

# Copy source and build
COPY src/ ./src/
RUN cargo build --release --locked

# Runtime stage - distroless non-root
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

# Copy binary from builder
COPY --from=builder /app/target/release/bridge /usr/local/bin/bridge

# Set non-root user (already default in distroless)
USER 65532:65532

# Metadata
VOLUME ["/state"]
ENTRYPOINT ["/usr/local/bin/bridge"]
CMD ["run"]
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/bridge", "health"]
