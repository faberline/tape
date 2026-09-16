# time 0.3.47 in Cargo.lock requires rustc 1.88 or newer. Keep the tester on
# the repository's current Bookworm toolchain convention.
FROM rust:1.92-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --locked --release -p tape --bin tape-bench --features jetstream-benchmark
FROM debian:bookworm-slim
COPY --from=build /src/target/release/tape-bench /usr/local/bin/tape-bench
ENTRYPOINT ["tape-bench", "jetstream-suite"]
