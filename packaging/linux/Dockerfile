# Build on the target architecture; buildx can use native Linux or QEMU.
FROM rust:1.95-bookworm AS build
WORKDIR /src
COPY scripts/linux-build-deps.sh scripts/
RUN bash scripts/linux-build-deps.sh
COPY Cargo.lock Cargo.toml LICENSE ./
COPY crates crates
COPY assets assets
COPY webviewer webviewer
COPY docs/clients/DESKTOP-APP.md docs/clients/TERMINAL.md docs/clients/
COPY packaging/linux/Velarouter.desktop packaging/linux/install.sh packaging/linux/
COPY scripts/package-linux.sh scripts/
RUN --mount=type=cache,id=vr-arm-registry,target=/usr/local/cargo/registry --mount=type=cache,id=vr-arm-target,target=/src/target bash scripts/package-linux.sh
FROM scratch
COPY --from=build /src/dist/ /
