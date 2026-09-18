FROM rust:1-bookworm AS builder

WORKDIR /usr/src/app

# Install build dependencies
RUN apt-get update && apt-get install -y pkg-config libssl-dev && rm -rf /var/lib/apt/lists/*

# Copy manifests first for dependency caching
COPY Cargo.toml Cargo.lock ./

# Create dummy source files to cache dependency compilation
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    echo "" > src/lib.rs && \
    mkdir -p src/bin && \
    echo "fn main() {}" > src/bin/mock_psp.rs && \
    cargo build --release || true

# Copy real source code and migrations
COPY src ./src
COPY migrations ./migrations

# Touch files to invalidate dummy build artifacts and build release binaries
RUN touch src/main.rs src/lib.rs src/bin/mock_psp.rs && \
    cargo build --release

# Runtime stage
FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates libssl3 curl postgresql-client && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Copy compiled binaries from builder stage
COPY --from=builder /usr/src/app/target/release/invoice-service /usr/local/bin/invoice-service
COPY --from=builder /usr/src/app/target/release/mock-psp /usr/local/bin/mock-psp

# Copy migrations folder for runtime migration execution
COPY migrations ./migrations

ENV RUST_LOG=info

EXPOSE 8080 8081

CMD ["invoice-service"]
