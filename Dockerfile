# Build stage
FROM rust:1.83-slim-bookworm AS builder

# Create a new empty shell project
WORKDIR /usr/src/app

# Install system dependencies
RUN apt-get update && \
    apt-get install -y pkg-config libssl-dev protobuf-compiler && \
    rm -rf /var/lib/apt/lists/*

# Copy manifests
COPY Cargo.toml Cargo.lock ./

# Copy crates
COPY crates ./crates

# Build protocol buffers first
RUN cargo build -p sova-sentinel-proto

# Build the full application
RUN cargo build --release -p sova-sentinel-server

# Final stage
FROM debian:bookworm-slim

# Install runtime dependencies
RUN apt-get update && \
    apt-get install -y ca-certificates libssl3 && \
    rm -rf /var/lib/apt/lists/*

# Create a non-root user with specific UID
RUN useradd -m -u 1001 sentinel

# Create data directory
# 755 means:
# - Give the sentinel user to read, write, and execute this directory.
# - Everyone else can only read and execute
RUN mkdir -p /app/data && chown sentinel:sentinel /app/data && chmod 755 /app/data

# Copy the binary from builder
COPY --from=builder /usr/src/app/target/release/sova-sentinel-server /usr/local/bin/

# Switch to the sentinel user
USER sentinel

# Set environment variables
ENV RUST_LOG=debug
ENV SOVA_SENTINEL_PORT=50051
ENV SOVA_SENTINEL_DB_PATH=/app/data/slot_locks.db
ENV BITCOIN_RPC_URL=http://localhost:8332
ENV BITCOIN_RPC_USER=user
ENV BITCOIN_RPC_PASS=pass
ENV BITCOIN_CONFIRMATION_THRESHOLD=6
ENV BITCOIN_REVERT_THRESHOLD=18

# Expose the service port
EXPOSE ${SOVA_SENTINEL_PORT}

# Run the binary
CMD ["sova-sentinel-server"]