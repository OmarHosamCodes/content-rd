FROM rust:1-slim AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY tests tests
RUN cargo build --release --locked

FROM debian:stable-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /src/target/release/content-rd /usr/local/bin/content-rd
# runs as root so the mounted volume (DATA_DIR) is writable
CMD ["content-rd"]
