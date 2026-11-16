# Builder stage
FROM rust:1.100.0-slim-bookworm@sha256:7c7d4eb4d05b7781187a327809466cfddf29753f6defdc3d19a0e291951b7a37 AS builder

WORKDIR /app

# Dependency layer: build all crates against stub sources so that this layer
# is reused while only src/ changes.
COPY Cargo.toml Cargo.lock ./
RUN mkdir src \
    && echo 'fn main() {}' > src/main.rs \
    && touch src/lib.rs \
    && cargo build --release --locked \
    && rm -rf src

# Real sources
COPY src/ ./src/
RUN touch src/main.rs src/lib.rs && cargo build --release --locked \
    && mkdir /state-skel
# Runtime stage - distroless non-root (glibc, CA certificates, no shell)
FROM gcr.io/distroless/cc-debian12:nonroot@sha256:9dac0a79194e45a7da0158a9c6da57b217585af0786db3845d1f0ec1a0dd182f

COPY --from=builder /app/target/release/bridge /usr/local/bin/bridge
# Owned by the runtime user so that a fresh named volume is writable.
COPY --from=builder --chown=65532:65532 /state-skel /state

USER 65532:65532

# The only writable location: SQLite database, WAL files and heartbeat file.
VOLUME ["/state"]
ENTRYPOINT ["/usr/local/bin/bridge"]
CMD ["run"]
STOPSIGNAL SIGTERM
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
    CMD ["/usr/local/bin/bridge", "healthcheck"]
