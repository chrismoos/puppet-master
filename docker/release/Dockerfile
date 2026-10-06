# syntax=docker/dockerfile:1.7

# Builds a release pm for the platform this image is built for, and
# exports just that binary. Unlike the sandbox worker image, the web
# bundle is copied in and embedded, because a release binary has to be
# able to serve the dashboard as well as run as a worker. Build the
# bundle on the host first: this stage does not run npm.
FROM rust:1.95-bookworm AS build

WORKDIR /src
ARG PM_GIT_REV=unknown

COPY --link Cargo.toml Cargo.lock ./
COPY --link crates ./crates
COPY --link proto ./proto
COPY --link web/dist ./web/dist

RUN --mount=type=cache,id=pm-release-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pm-release-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=pm-release-cargo-target,target=/src/target \
    PM_GIT_REV="$PM_GIT_REV" cargo build --locked --release -p pm \
    && install -Dm755 target/release/pm /out/pm

FROM scratch
COPY --from=build /out/pm /pm
