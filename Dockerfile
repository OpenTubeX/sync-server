FROM lukemathwalker/cargo-chef:latest-rust-alpine AS chef
WORKDIR /app

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN apk add sqlite-static sqlite-dev openssl-libs-static openssl-dev
RUN cargo chef cook --release --recipe-path recipe.json

# Build application
COPY . .
ARG DATABASE_BACKEND=sqlite

RUN cargo build --release --locked --no-default-features --features "${DATABASE_BACKEND}"

FROM alpine:latest AS runner
WORKDIR /app

# required for connecting to YouTube for input data validation
RUN apk add ca-certificates

COPY --from=builder /app/target/release/opentubex-sync /app/opentubex-sync-server

# Run unprivileged.
#
# NOTE for SQLite deployments: a bind-mounted host directory keeps its host
# ownership and shadows the ownership set here, so the host directory must be
# owned by this uid or the database and its WAL sidecars are not writable:
#     sudo chown -R 10001:10001 ./data
# See the "Running as non-root" section in README.md.
RUN addgroup -S -g 10001 opentubex \
    && adduser -S -u 10001 -G opentubex opentubex \
    && mkdir -p /app/data \
    && chown -R opentubex:opentubex /app
USER 10001:10001

EXPOSE 8080
CMD ["./opentubex-sync-server"]
