# Builder stage
FROM rust:1.100.0-slim-bookworm@sha256:f5c7843d41f27d0b9b7c1b8d5a91f8e6c8d0e9f8b6c5a4d3e2f1a0b9c8d7e

WORKDIR /app

# Copy Cargo files first for layer caching
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch

# Copy source and build
COPY src/ ./src/
RUN cargo build --release --locked

# Runtime stage - distroless non-root
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:d7c9e8f7b6a5d4c3e2f1a0b9c8d7e6f5a4d3c2b1e0f9a8b7c6d5e4f3a2b1c0

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
    CMD ["/usr/local/bin/bridge", "health"] || exit 1
