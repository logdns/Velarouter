FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo build --locked --release -p vr-signal --bin vr-signal

FROM debian:bookworm-slim
COPY --from=build /src/target/release/vr-signal /usr/local/bin/vr-signal
USER 65534:65534
ENTRYPOINT ["vr-signal"]
