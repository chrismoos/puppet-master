# Runtime image for the push gateway relay (`pm pushgw`).
#
# Uses no BuildKit-only features, so it builds under the legacy builder
# too. Build it for the host it will run on:
#
#   docker build --platform linux/amd64 -f deploy/pushgw/Dockerfile -t pushgw .
#
# The build context is the repository root, where .dockerignore already
# drops target/ and node_modules/.

FROM rust:1.95-bookworm AS build

WORKDIR /src
ARG PM_GIT_REV=unknown

COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY proto ./proto

# web/dist is left out: the relay serves only /v1/push and /health, so
# embedding the dashboard would grow the binary for nothing.
RUN PM_GIT_REV="$PM_GIT_REV" cargo build --locked --release -p pm \
    && install -Dm755 target/release/pm /out/pm

FROM gcr.io/distroless/cc-debian12:nonroot

COPY --from=build /out/pm /usr/local/bin/pm

# The relay binds loopback by default, which a container would make
# unreachable.
ENV PUSHGW_LISTEN=0.0.0.0:8080
EXPOSE 8080

ENTRYPOINT ["/usr/local/bin/pm", "pushgw"]
