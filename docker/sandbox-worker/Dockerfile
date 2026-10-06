# syntax=docker/dockerfile:1.7

# Where /opt/pm/bin/pm comes from. `compile` builds it from this tree,
# which is what `make sandbox-image` does and the only option that can
# carry an unreleased change. `prebuilt` takes the binary the release
# already built for this architecture.
ARG PM_BINARY=compile

# Build pm for the container's Linux OS and architecture. BuildKit cache
# mounts keep dependencies and incremental objects across source rebuilds,
# while the compiler and its output cache stay out of the runtime image.
FROM rust:1.95-bookworm AS pm-compile

WORKDIR /src
ARG PM_GIT_REV=unknown

COPY --link Cargo.toml Cargo.lock ./
COPY --link crates ./crates
COPY --link proto ./proto

RUN --mount=type=cache,id=pm-sandbox-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=pm-sandbox-cargo-git,target=/usr/local/cargo/git \
    --mount=type=cache,id=pm-sandbox-cargo-target,target=/src/target \
    PM_GIT_REV="$PM_GIT_REV" cargo build --locked --release -p pm \
    && install -Dm755 target/release/pm /out/pm

# The release's own artifact for this architecture, staged by
# scripts/publish.mjs. Taking it instead of compiling again keeps the
# image's pm identical to the signed download a host would otherwise
# update itself to, and keeps a two-architecture build from emulating a
# second release compile. It carries the web bundle, which a worker never
# serves and the compiled stage therefore leaves out.
FROM scratch AS pm-prebuilt
ARG TARGETARCH
COPY dist/release/image/${TARGETARCH}/pm /out/pm

FROM pm-${PM_BINARY} AS pm-binary

# Sandboxed worker runtime for `pm worker --sandbox`.
FROM ubuntu:25.10

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        git \
        nodejs \
        npm \
        openssh-client \
        ripgrep \
        sudo \
    && rm -rf /var/lib/apt/lists/*

RUN npm install -g @anthropic-ai/claude-code @openai/codex

COPY --from=pm-binary /out/pm /opt/pm/bin/pm

# The worker needs a writable HOME (enrollment config, agent state) and
# a writable temp dir (hook socket and session files live under one). It
# also owns /opt/pm: following the controller's build writes a new binary
# beside the running one and renames over it, which needs the directory
# and not just the file.
RUN if id ubuntu >/dev/null 2>&1; then userdel -r ubuntu; fi \
    && useradd -m -u 1000 -s /bin/bash worker \
    && chown -R worker:worker /opt/pm

# The container is the boundary, so the worker holds root inside it. The
# drop-in is checked before it becomes one sudo reads: a malformed file
# makes sudo refuse to run at all. This keeps the image in step with the
# Incus provisioner in crates/pm/src/sandbox.rs, which grants the same
# rule the same way.
RUN printf '%s\n' 'worker ALL=(ALL) NOPASSWD:ALL' > /etc/pm-worker.sudoers.new \
    && visudo -cf /etc/pm-worker.sudoers.new \
    && install -m 0440 -o root -g root /etc/pm-worker.sudoers.new /etc/sudoers.d/pm-worker \
    && rm -f /etc/pm-worker.sudoers.new

USER worker
ENV HOME=/home/worker
WORKDIR /home/worker
