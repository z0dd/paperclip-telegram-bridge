# Builder stage
FROM rust:1.100.0-slim-bookworm AS builder

WORKDIR /app

# Copy Cargo files first for layer caching
COPY Cargo.toml Cargo.lock ./
RUN cargo fetch

# Copy source and build
COPY src/ ./src/
RUN cargo build --release --locked

# Runtime stage - distroless non-root
FROM gcr.io/distroless/cc-debian12:nonroot

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
