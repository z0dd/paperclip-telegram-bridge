# Builder stage
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder

WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src/ ./src/

# Install dependencies for bundled features if needed
RUN apt-get update && apt-get install -y --no-install-recommends \
    gcc \
    libc6-dev \
    && rm -rf /var/lib/apt/lists/*

RUN cargo build --release --locked

# Runtime stage
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:3d37b176f531e3f51326b5891a6fcc0b0c87318928b09049185aff28dae5426e

WORKDIR /app

# Copy binary from builder
COPY --from=builder /app/target/release/bridge /usr/local/bin/bridge

# Create state directory
RUN mkdir -p /state && chmod 777 /state

VOLUME ["/state"]

# Non-root user is already 65532:65532 in distroless
USER 65532:65532

ENTRYPOINT ["/usr/local/bin/bridge"]
CMD ["run"]

# Health check
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/usr/local/bin/bridge", "healthcheck"] || exit 1

# Signal handling
STOPSIGNAL SIGTERM
