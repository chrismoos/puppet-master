# Puppet Master build and run.
#
# The release binary is the default for running: it is much faster than
# a debug build, which matters for the daemon and the agents it drives.
# The test lanes are optimized too, under the `e2e` profile: link-time
# optimization runs per linked binary and the Rust suite links about thirty
# of them, so one set of artifacts serves the Rust, browser, and fixture
# lanes. `make e2e-performance` keeps the thin-LTO release binary.
#
# On a slow network filesystem, keep build output on local disk with
# `make CARGO_TARGET_DIR=$HOME/.cache/pm-build <target>` (or export it).

CARGO ?= cargo
PNPM ?= pnpm
ifeq ($(shell uname),Linux)
CARGO_TARGET_DIR ?= $(HOME)/.cache/pm-build
else
CARGO_TARGET_DIR ?= target
endif
export CARGO_TARGET_DIR

BIN := $(CARGO_TARGET_DIR)/release/pm
# Loopback by default. The web UI is full control over agent terminals, so
# widening it is an explicit choice: make run HTTP=0.0.0.0:7676
HTTP ?= 127.0.0.1:7676
E2E_SPECS ?=
SANDBOX_IMAGE ?= puppet-master-worker:local
SANDBOX_PLATFORM ?=
LINUX_X86_64_IMAGE ?= puppet-master-worker:linux-x86_64
LINUX_X86_64_BIN ?= dist/pm-linux-x86_64
PUSHGW_IMAGE ?= pm-pushgw:local
PUSHGW_PLATFORM ?=
CONTAINER_RUNTIME ?= docker
GIT_REV := $(shell git rev-parse --short HEAD 2>/dev/null || echo unknown)$(shell test -z "$$(git status --porcelain 2>/dev/null)" || echo -dirty)

.PHONY: help build web rust run dev ios-run ios-build ios-device ios-reset ios-testflight ios-bump ios-version ios-testflight-test ios-doctor ios-prebuild ios-bundle ios-icon ios-clean test rust-test web-test publish promote publish-test install-test e2e-worker e2e-integration e2e-terminal-heavy e2e-performance browser-e2e e2e-contract-test e2e-source-check controller-fixture controller-fixture-test controller-fixture-live-test check fmt fmt-check clippy lint proto clean sandbox-image pushgw-image linux-x86_64 sandbox-container-test sandbox-incus-test licenses licenses-check

help: ## Show this help.
	@grep -hE '^[a-z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | sort | \
	  awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1m%-12s\033[0m %s\n", $$1, $$2}'

build: web rust ## Build the web bundle and the release binary.

web: node_modules ## Build the web UI bundle into web/dist.
	$(PNPM) -F puppet-master-web run build

node_modules: package.json pnpm-lock.yaml pnpm-workspace.yaml web/package.json packages/client-core/package.json apps/mobile/package.json
	$(PNPM) install
	@touch node_modules

rust: ## Build the release pm binary (embeds web/dist; run `web` first).
	$(CARGO) build --release

run: build ## Build everything and run the daemon (release) with the web UI.
	$(BIN) daemon --http $(HTTP)

dev: node_modules ## Frontend dev server (hot reload); needs a daemon running separately.
	$(PNPM) -F puppet-master-web run dev

# The iOS app is built from a generated Xcode project and a generated
# WebView bundle, so its targets live in apps/mobile/Makefile where those
# dependencies can be expressed. These are the way in; `make -C apps/mobile
# help` lists the rest. Needs a Mac with Xcode.
ios-run: ## Build, install, and launch the iOS app on a simulator (DEVICE=<name>).
	$(MAKE) -C apps/mobile run

ios-build: ## Compile the iOS app for the simulator without installing it.
	$(MAKE) -C apps/mobile build

ios-device: ## Release build of the iOS app onto a connected device (DEVICE=<udid>).
	$(MAKE) -C apps/mobile device

ios-reset: ## Discard the generated iOS project and installed app, then build and run.
	$(MAKE) -C apps/mobile reset

ios-testflight: ## Increment the saved iOS build number, archive, and upload to TestFlight.
	@$(MAKE) -C apps/mobile testflight

ios-bump: ## Increment the iOS build number in app.json and commit it.
	@$(MAKE) -C apps/mobile bump

ios-version: ## Set the iOS App Store version in app.json and commit it (VERSION=1.1.0).
	@$(MAKE) -C apps/mobile version VERSION=$(VERSION)

ios-doctor: ## Report which iOS build inputs are present and what to run.
	$(MAKE) -C apps/mobile doctor

ios-testflight-test: ## Test how the TestFlight script assembles its build.
	@$(MAKE) -C apps/mobile testflight-test

ios-prebuild: ## Generate the iOS project and install pods.
	$(MAKE) -C apps/mobile prebuild

ios-bundle: ## Generate the WebView terminal bundle the mobile app resolves.
	$(MAKE) -C apps/mobile bundle

ios-icon: ## Render the mobile app icon from its SVG source.
	$(MAKE) -C apps/mobile icon

ios-clean: ## Remove the generated iOS project and build output.
	$(MAKE) -C apps/mobile clean

# Agent CLIs record which directories they trust in a config under the
# user's home. Test lanes spawn agents in throwaway directories, so they
# answer into a throwaway home instead of the developer's real one, which
# a parallel lane would otherwise corrupt for every agent on the host.
test rust-test web-test e2e-worker e2e-integration e2e-terminal-heavy e2e-performance browser-e2e: \
	export CODEX_HOME = $(abspath $(CARGO_TARGET_DIR))/test-agent-home/codex

test: rust-test web-test ## Run the full test suite (Rust + web).

rust-test: ## Run the Rust test suite (optimized, non-LTO, via nextest).
	$(CARGO) nextest run --cargo-profile e2e --no-fail-fast --workspace --features pm-daemon/testagent

web-test: node_modules ## Run the shared client-core, web, and mobile test suites.
	$(PNPM) -F @puppet-master/client-core run test
	$(PNPM) -F puppet-master-web run test
	$(PNPM) -F puppet-master-mobile run test

e2e-worker e2e-integration e2e-terminal-heavy e2e-performance: node_modules

e2e-worker: ## Run focused functional Chromium without release LTO (set E2E_SPECS).
	CARGO="$(CARGO)" PNPM="$(PNPM)" ./scripts/e2e-lane.mjs worker $(E2E_SPECS)

e2e-integration: ## Run the parallel fast functional Chromium suite.
	CARGO="$(CARGO)" PNPM="$(PNPM)" ./scripts/e2e-lane.mjs integration

e2e-terminal-heavy: ## Run high-volume terminal Chromium scenarios in isolation.
	CARGO="$(CARGO)" PNPM="$(PNPM)" ./scripts/e2e-lane.mjs terminal-heavy

e2e-performance: ## Run serialized release Chromium performance budgets.
	CARGO="$(CARGO)" PNPM="$(PNPM)" ./scripts/e2e-lane.mjs performance

browser-e2e: e2e-integration ## Compatibility alias for the standard functional Chromium lane.

e2e-contract-test: ## Test E2E lane selection and binary/profile wiring.
	node --test scripts/e2e-lane.test.mjs

e2e-source-check: ## Reject order-dependent E2E names and undocumented fixed waits.
	node scripts/check-e2e-source.mjs
	node --test scripts/check-e2e-source.test.mjs

# One command for a client suite that needs a real controller: build the
# daemon and the scripted agent, then run a seeded throwaway controller in
# the foreground and print its connection details as JSON. Ctrl-C stops the
# daemon and removes everything it wrote.
#
# The recipe is silent and the build writes to stderr, because a consumer
# parses this target's stdout: an echoed recipe line or a "Compiling" note
# in front of the document breaks it.
controller-fixture: ## Run a seeded throwaway controller for client tests (FIXTURE_ARGS=...).
	@$(CARGO) build --profile e2e --features pm-daemon/testagent --bin pm --bin pm-testagent 1>&2
	@./scripts/controller-fixture.mjs run $(FIXTURE_ARGS)

controller-fixture-test: ## Test the fixture's path isolation, readiness, shape, and diagnostics.
	node --test scripts/controller-fixture.test.mjs

# The enrollment contract is the daemon's own behaviour, so proving it
# needs a real controller and therefore a build. That keeps it out of
# `check`, which must stay fast and buildless.
controller-fixture-live-test: ## Test the fixture's enrollment contract against a real controller.
	$(CARGO) build --profile e2e --features pm-daemon/testagent --bin pm --bin pm-testagent
	node --test scripts/controller-fixture.live.test.mjs

publish-test: ## Test release version bumping and the manifest contract.
	node --test scripts/publish.test.mjs

install-test: ## Test the installer's PATH setup.
	node --test scripts/install.test.mjs

# The version is the script's to compute: it reads the current one, bumps
# it, and records it. Nothing here should have to look it up. An empty
# BUMP means patch for a stable release and, for a channel build, the
# stable the current pre-release already heads for.
BUMP ?=

publish: ## Cut the next release. Run this on macOS. BUMP=minor|major to go further, CHANNEL=dev for a pre-release on that channel, PUBLISH_FLAGS to pass the script its own flags.
	./scripts/publish.mjs $(BUMP) $(if $(CHANNEL),--channel $(CHANNEL)) $(PUBLISH_FLAGS)

promote: ## Cut the stable a pre-release leads to, from its commit: FROM=0.10.0-dev.3. Run this on macOS.
	./scripts/publish.mjs promote $(FROM) $(PUBLISH_FLAGS)

check: fmt-check clippy web-check e2e-source-check controller-fixture-test publish-test install-test ios-testflight-test ## Fast checks: fmt, clippy, web typecheck, E2E source policy, and release tooling.

web-check: node_modules
	$(PNPM) -F @puppet-master/client-core run check
	$(PNPM) -F puppet-master-web run check
	$(PNPM) -F puppet-master-mobile run check

fmt: ## Format Rust code.
	$(CARGO) fmt --all

fmt-check:
	$(CARGO) fmt --all --check

clippy: ## Lint Rust code (release, warnings are errors).
	$(CARGO) clippy --release --workspace --all-targets --features pm-daemon/testagent -- -D warnings

sandbox-image: ## Build the worker image from this tree to the local tag puppet-master-worker:local.
	$(CONTAINER_RUNTIME) build$(if $(strip $(SANDBOX_PLATFORM)), --platform $(SANDBOX_PLATFORM)) --build-arg PM_GIT_REV=$(GIT_REV) -t $(SANDBOX_IMAGE) -f docker/sandbox-worker/Dockerfile .

pushgw-image: ## Build the push gateway relay image to the local tag pm-pushgw:local.
	$(CONTAINER_RUNTIME) build$(if $(strip $(PUSHGW_PLATFORM)), --platform $(PUSHGW_PLATFORM)) --build-arg PM_GIT_REV=$(GIT_REV) -t $(PUSHGW_IMAGE) -f deploy/pushgw/Dockerfile .

linux-x86_64: SANDBOX_PLATFORM = linux/amd64
linux-x86_64: SANDBOX_IMAGE = $(LINUX_X86_64_IMAGE)
linux-x86_64: sandbox-image ## Build Linux x86_64 and export the pm binary to dist/.
	@mkdir -p "$(dir $(LINUX_X86_64_BIN))"
	@set -eu; \
	  container_id=""; \
	  cleanup() { test -z "$$container_id" || $(CONTAINER_RUNTIME) rm "$$container_id" >/dev/null 2>&1 || :; }; \
	  trap cleanup EXIT INT TERM; \
	  container_id="$$( $(CONTAINER_RUNTIME) create --platform "$(SANDBOX_PLATFORM)" "$(SANDBOX_IMAGE)" )"; \
	  $(CONTAINER_RUNTIME) cp "$$container_id:/opt/pm/bin/pm" "$(LINUX_X86_64_BIN)"; \
	  chmod +x "$(LINUX_X86_64_BIN)"; \
	  echo "Wrote $(LINUX_X86_64_BIN)"

sandbox-container-test: ## Run the container-executing sandbox tests (needs the sandbox image).
	PM_SANDBOX_CONTAINER_TESTS=1 $(CARGO) nextest run -p pm --test sandbox_container

sandbox-incus-test: ## Run the Incus-executing sandbox tests (needs a reachable incus).
	PM_SANDBOX_INCUS_TESTS=1 $(CARGO) nextest run -p pm --test sandbox_incus

licenses: node_modules ## Regenerate the third-party license inventory.
	node scripts/third-party-licenses.mjs

licenses-check: licenses ## Fail when the committed inventory is out of date.
	git diff --exit-code -- licenses/THIRD-PARTY.md

proto: ## Lint the protobuf contract and regenerate the shared client types.
	buf lint proto
	$(PNPM) -F @puppet-master/client-core run proto:gen

lint: fmt-check clippy ## Alias for the Rust lints.

clean: ## Remove build output.
	$(CARGO) clean
	rm -rf web/dist node_modules web/node_modules packages/client-core/node_modules apps/mobile/node_modules
