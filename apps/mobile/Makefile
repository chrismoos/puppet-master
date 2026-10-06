# Building and running the iOS app.
#
# A checkout does not carry the things the app needs to run: installed
# dependencies, the generated WebView terminal bundle, and the prebuilt
# Xcode project. Each target below builds what it needs first, so a fresh
# checkout reaches a running app in one command instead of a sequence
# nobody remembers.
#
#   make build     compile for the simulator, install nothing
#   make run       build, install, and launch on a simulator
#   make device    Release build onto a connected device
#   make reset     discard everything generated, then build and run
#   make doctor    what is present, what is missing, and what to run
#   make testflight  archive locally and upload the build to TestFlight
#
# Pass TESTFLIGHT_FLAGS=--dry-run to see what a TestFlight run would do,
# --no-upload to stop at the exported IPA, or --from-archive to re-export
# and upload the archive already built rather than building another.
#
# The export signs with the distribution profiles already installed.
# --automatic-signing hands that back to Xcode, which resolves them
# against the portal instead.
#
# Override CONFIGURATION=Debug on any target for a debug build; a device
# build is Release unless you do. Dev-only diagnostics compile out of a
# Release build, so reach for `make ios-device CONFIGURATION=Debug` when
# you need them on hardware.
#
# Pass DEVICE to pick a target: `make run DEVICE="iPhone 17 Pro"`, or
# `make device DEVICE=<udid>`. Without it, run uses the booted simulator
# and device prompts for a choice.
#
# The root Makefile spells each of these ios-run, ios-reset and so on.
#
#   make uitest-full  XCUITests (bootstraps on first run, ~38 s warm reuse)

PNPM ?= pnpm
SCHEME ?= PuppetMaster
CONFIGURATION ?= Debug
DEVICE ?=
BUNDLE_ID ?= com.tech9.puppetmaster

WORKSPACE := ios/$(SCHEME).xcworkspace
PODS := ios/Podfile.lock
PROVIDER := ios/Pods/Target Support Files/Pods-$(SCHEME)/ExpoModulesProvider.swift
TERMINAL_BUNDLE := src/terminal/gen/terminalHtml.ts
ICON := assets/icon.png
NODE_MODULES := ../../node_modules
# Signing and upload identity. Nothing here is secret, but all of it is
# per-developer, so it comes from the environment rather than the repo.
# The App Store Connect .p8 stays outside the checkout entirely.
TEAM_ID ?= $(APPLE_TEAM_ID)
ASC_KEY_ID ?= $(APP_STORE_CONNECT_KEY_ID)
ASC_ISSUER_ID ?= $(APP_STORE_CONNECT_ISSUER_ID)
DERIVED ?= ios/build

# Sources the Expo config plugin copies into ios/ during prebuild. Make
# does not re-run prebuild when only these change (the prerequisites are
# app.json and package.json), so the copied files go stale. Syncing them
# before every build is cheap and prevents silent use of old code.
NSE_SRC := plugins/push-crypto/ios/NotificationService.swift
NSE_DST := ios/PuppetMasterNSE/NotificationService.swift

.PHONY: help build run device reset prebuild prebuild-clean bundle icon doctor clean testflight testflight-check testflight-test sync-nse uitest uitest-full bump version

# Advice printed by these targets is read from either directory, and the
# root Makefile spells them ios-run, ios-reset and so on. Accepting both
# spellings costs one rule and removes a class of "that is not a target"
# from every message this file prints.
ios-%:
	@$(MAKE) $*

help: ## Show this help.
	@grep -hE '^[a-z0-9_-]+:.*?## ' $(MAKEFILE_LIST) | sort | \
	  awk 'BEGIN {FS = ":.*?## "} {printf "  \033[1m%-16s\033[0m %s\n", $$1, $$2}'

build: prebuild sync-nse ## Compile for the simulator without installing it.
	xcodebuild -workspace $(WORKSPACE) -scheme $(SCHEME) \
	  -configuration $(CONFIGURATION) -sdk iphonesimulator \
	  -destination 'generic/platform=iOS Simulator' \
	  -derivedDataPath $(DERIVED) build

run: prebuild sync-nse ## Build, install, and launch on a simulator.
	$(PNPM) exec expo run:ios $(if $(DEVICE),--device "$(DEVICE)")

device: CONFIGURATION = Release
device: prebuild sync-nse ## Build onto a connected device, Release by default (needs signing set up in Xcode).
	$(PNPM) exec expo run:ios --configuration $(CONFIGURATION) --device $(if $(DEVICE),"$(DEVICE)")

sync-nse: ## Copy the NSE source into the generated project if it has changed.
	@test -d ios/PuppetMasterNSE && \
	  if ! cmp -s $(NSE_SRC) $(NSE_DST) 2>/dev/null; then \
	    cp $(NSE_SRC) $(NSE_DST); \
	    echo "synced NotificationService.swift into ios/"; \
	  fi || true

icon: $(ICON) ## Render the app icon from its SVG (needs rsvg-convert).

# iOS wants one opaque 1024px square and masks the corners itself, so the
# source tile is square and the render is flattened onto the tile colour
# rather than left with an alpha channel the App Store rejects.
$(ICON): assets/icon.svg
	rsvg-convert -w 1024 -h 1024 -b '#0B1422' assets/icon.svg -o $(ICON)

bundle: $(TERMINAL_BUNDLE) ## Generate the WebView terminal bundle Metro resolves.

$(TERMINAL_BUNDLE): $(NODE_MODULES) terminal-web/build.mjs terminal-web/index.html $(wildcard terminal-web/src/*.ts)
	$(PNPM) run build:terminal

prebuild: $(PODS) ## Generate ios/ and install pods, if the app config or dependencies moved.

# The native project is generated from app.json and the installed module
# set rather than edited, so it is rebuilt whenever either moves.
#
# The registry is removed first because Expo generates it once and then
# leaves it alone: a prebuild over a project created before a module was
# added keeps the old ExpoModulesProvider.swift, and the app builds,
# links, launches and registers nothing new, with nothing in the build to
# say so. Pods are installed unconditionally for the same reason —
# prebuild runs pod install only when it judges the Podfile changed,
# which installing a dependency does not always do.
$(PODS): app.json package.json $(TERMINAL_BUNDLE) $(ICON) $(NODE_MODULES)
	@rm -f "$(PROVIDER)"
	$(PNPM) exec expo prebuild -p ios
	cd ios && pod install
	@test -f $(PODS) && touch $(PODS)

prebuild-clean: bundle $(NODE_MODULES) ## Regenerate ios/ from scratch, discarding local edits to it.
	$(PNPM) exec expo prebuild -p ios --clean

$(NODE_MODULES): ../../package.json ../../pnpm-lock.yaml ../../pnpm-workspace.yaml package.json
	cd ../.. && $(PNPM) install
	@touch $(NODE_MODULES)

doctor: ## Report which build inputs are present and what to run for the missing ones.
	@xcode=$$(xcodebuild -version 2>/dev/null | head -1); echo "xcodebuild:     $${xcode:-not installed}"
	@test -d $(NODE_MODULES) \
	  && echo "node modules:   present" \
	  || echo "node modules:   MISSING -> make ios-run"
	@test -f $(TERMINAL_BUNDLE) \
	  && echo "web bundle:     present" \
	  || echo "web bundle:     MISSING -> make ios-bundle  (Metro cannot resolve the terminal without it)"
	@test -d ios \
	  && echo "ios project:    present" \
	  || echo "ios project:    MISSING -> make ios-run"
	@test -f $(PODS) \
	  && echo "pods:           installed" \
	  || echo "pods:           MISSING -> make ios-prebuild"
	@if [ -f $(PODS) ] && [ package.json -nt $(PODS) ]; then \
	  echo "pods:           STALE, package.json is newer -> make ios-prebuild"; fi
	@test -f "$(PROVIDER)" \
	  && echo "registry:       present" \
	  || echo "registry:       missing -> make ios-prebuild"
	@test -n "$(TEAM_ID)" \
	  && echo "team id:        $(TEAM_ID)" \
	  || echo "team id:        unset -> export APPLE_TEAM_ID=...  (TestFlight only)"
	@ls ~/.appstoreconnect/private_keys/AuthKey_$(ASC_KEY_ID).p8 >/dev/null 2>&1 \
	  && echo "asc key:        present" \
	  || echo "asc key:        missing -> ~/.appstoreconnect/private_keys/  (TestFlight only)"
	@echo "build number:   $(if $(BUILD_NUMBER),$(BUILD_NUMBER),increment app.json)"

reset: ## Throw away everything generated and the installed app, then build and run from scratch.
	rm -rf ios $(DERIVED)
	-xcrun simctl uninstall booted $(BUNDLE_ID)
	$(MAKE) run

testflight-check: ## Report what the TestFlight path is missing, without building.
	@node scripts/testflight.mjs --check

testflight-test: ## Test how the TestFlight script assembles its build.
	@node --test scripts/testflight.test.mjs

# The script drives prebuild itself: the build number has to reach
# app.json before the project that carries it is generated, and make
# does not order two prerequisites of the same target under -j.
testflight: ## Increment the saved build number, archive, and upload to TestFlight.
	MAKE="$(MAKE)" node scripts/testflight.mjs $(if $(BUILD_NUMBER),--build-number $(BUILD_NUMBER)) $(TESTFLIGHT_FLAGS)

bump: ## Increment the iOS build number in app.json and commit it.
	node scripts/testflight.mjs --bump-only $(if $(BUILD_NUMBER),--build-number $(BUILD_NUMBER))

version: ## Set the App Store version in app.json and commit it (VERSION=1.1.0).
	@test -n "$(VERSION)" || { echo "usage: make version VERSION=1.1.0"; exit 1; }
	node scripts/testflight.mjs --set-version $(VERSION)

# ── XCUITest ─────────────────────────────────────────────────────
#
# Runs the XCUITest geometric assertions against a simulator that is
# already booted and enrolled against a throwaway daemon serving a plan
# decision with a custom option.
#
# Prerequisites:
#   1. A throwaway daemon bound to a loopback port with a test user.
#   2. The app built and installed as a UI-test build:
#        PM_UI_TEST_BUILD=1 PM_DEV_CONTROLLER_URL=http://127.0.0.1:<port> \
#          make prebuild-clean
#        make build DEVICE=<udid>
#   3. A plan decision with custom option seeded on the daemon.
#
# Invocation:
#   make uitest DEVICE=<udid>
#
# Missing seeded decisions fail the test.

uitest: prebuild sync-uitest ## Run XCUITest geometric assertions on a simulator.
	xcodebuild test \
	  -workspace $(WORKSPACE) -scheme PuppetMasterUITests \
	  -destination "id=$(if $(DEVICE),$(DEVICE),$(shell xcrun simctl list devices booted -j | python3 -c 'import json,sys;ds=[d for r in json.load(sys.stdin)["devices"].values() for d in r if d["state"]=="Booted"];print(ds[0]["udid"] if ds else "booted")'))" \
	  -derivedDataPath $(DERIVED) \
	  -only-testing:PuppetMasterUITests

sync-uitest: ## Copy UI test sources into the generated project if they changed.
	@if [ -d ios/PuppetMasterUITests ]; then \
	  for f in plugins/ui-tests/ios/*.swift; do \
	    dst="ios/PuppetMasterUITests/$$(basename $$f)"; \
	    if ! cmp -s "$$f" "$$dst" 2>/dev/null; then \
	      cp "$$f" "$$dst"; \
	      echo "synced $$(basename $$f) into ios/PuppetMasterUITests/"; \
	    fi; \
	  done; \
	fi

uitest-full: ## Bootstrap or reuse an isolated test environment and run XCUITests.
	./scripts/run-uitest.sh $(if $(ONLY_TESTING),--only $(ONLY_TESTING))

clean: ## Remove the generated project and build output.
	rm -rf ios $(DERIVED)
