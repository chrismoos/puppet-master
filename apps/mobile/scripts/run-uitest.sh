#!/usr/bin/env bash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
MOBILE_DIR="$REPO_ROOT/apps/mobile"
ENV_DIR="$MOBILE_DIR/.uitest-env"

ONLY_TESTING=""
CLEAN=false
DESTROY=false
DEVICE_TYPE="com.apple.CoreSimulator.SimDeviceType.iPhone-16"
RUNTIME=""
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$HOME/.cache/pm-build-workers/mobile-shared}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --only)        ONLY_TESTING="$2"; shift 2 ;;
    --clean)       CLEAN=true; shift ;;
    --destroy)     DESTROY=true; shift ;;
    --device-type) DEVICE_TYPE="$2"; shift 2 ;;
    --runtime)     RUNTIME="$2"; shift 2 ;;
    -h|--help)
      cat <<'USAGE'
Usage: run-uitest.sh [--only Class] [--clean] [--destroy] [--device-type ID] [--runtime ID]

Bootstraps or reuses an isolated test environment (.uitest-env/), then
runs XCUITests via test-without-building.

  --only Class   Run only tests in the named XCUITest class.
  --clean        Destroy the cached environment and bootstrap from scratch.
  --destroy      Stop fixture, delete simulator, remove .uitest-env/ and
                 ios/. Does not rebuild or run tests.
  --device-type  Simulator device type (default: iPhone-16).
  --runtime      Simulator runtime (default: latest available iOS).

First run bootstraps (~15 min): builds pm binary if absent, creates a
simulator, starts a fixture daemon, prebuilds the Xcode project, and
compiles a Release app. Subsequent runs reuse everything and call
test-without-building directly (~38 s). Changed test sources trigger
only a test-runner rebuild (a few seconds).

If the fixture dies between runs the harness asks for --clean.

Respects CARGO_TARGET_DIR if set. Defaults to the shared mobile cache
at ~/.cache/pm-build-workers/mobile-shared.
USAGE
      exit 0 ;;
    *)             echo "unknown option: $1" >&2; exit 1 ;;
  esac
done

die() { echo "FATAL: $1" >&2; exit 1; }

destroy_env() {
  [[ -f "$ENV_DIR/state.json" ]] || return 0
  local udid froot
  udid="$(python3 -c 'import json; print(json.load(open("'"$ENV_DIR"'/state.json"))["udid"])' 2>/dev/null || true)"
  froot="$(python3 -c 'import json; print(json.load(open("'"$ENV_DIR"'/state.json"))["fixture_root"])' 2>/dev/null || true)"
  if [[ -n "$udid" ]]; then
    xcrun simctl shutdown "$udid" 2>/dev/null || true
    xcrun simctl delete "$udid" 2>/dev/null || true
  fi
  if [[ -n "$froot" ]]; then
    node "$REPO_ROOT/scripts/controller-fixture.mjs" stop --root "$froot" 2>/dev/null || true
    rm -rf "$froot"
  fi
  rm -rf "$ENV_DIR" "$MOBILE_DIR/ios"
}

if [[ "$DESTROY" == true ]]; then
  destroy_env
  echo "destroyed"
  exit 0
fi

if [[ "$CLEAN" == true ]]; then
  destroy_env
fi

if [[ -z "$RUNTIME" ]]; then
  RUNTIME="$(xcrun simctl list runtimes -j \
    | python3 -c 'import json,sys; rs=[r for r in json.load(sys.stdin)["runtimes"] if r["isAvailable"] and "iOS" in r["name"]]; print(rs[-1]["identifier"] if rs else "")')"
  [[ -n "$RUNTIME" ]] || die "no iOS simulator runtime"
fi

export CARGO_TARGET_DIR
mkdir -p "$CARGO_TARGET_DIR"

PM_BIN=""
for profile in e2e release debug; do
  candidate="$CARGO_TARGET_DIR/$profile/pm"
  if [[ -x "$candidate" ]]; then PM_BIN="$candidate"; break; fi
done

if [[ -z "$PM_BIN" ]]; then
  echo "pm binary not found in $CARGO_TARGET_DIR, building (e2e profile)..."
  cargo build --profile e2e --features pm-daemon/testagent --bin pm --bin pm-testagent 2>&1 | tail -5
  PM_BIN="$CARGO_TARGET_DIR/e2e/pm"
  [[ -x "$PM_BIN" ]] || die "cargo build succeeded but $PM_BIN not found"
  echo "pm binary ready"
fi

cd "$MOBILE_DIR"

if [[ ! -f "$ENV_DIR/state.json" ]]; then
  mkdir -p "$ENV_DIR"

  UDID="$(xcrun simctl create "pm-uitest-wt" "$DEVICE_TYPE" "$RUNTIME")"
  xcrun simctl boot "$UDID"
  echo "simulator $UDID"

  FIXTURE_JSON="$(node "$REPO_ROOT/scripts/controller-fixture.mjs" start --format json 2>/dev/null)"
  FIXTURE_ROOT="$(echo "$FIXTURE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["root"])')"
  FIXTURE_URL="$(echo "$FIXTURE_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["baseUrl"])')"
  echo "fixture $FIXTURE_URL"

  TOKEN_JSON="$(node "$REPO_ROOT/scripts/controller-fixture.mjs" enroll-token --root "$FIXTURE_ROOT" 2>/dev/null)"
  ENROLL_TOKEN="$(echo "$TOKEN_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["token"])')"

  PM_UI_TEST_BUILD=1 \
    PM_DEV_CONTROLLER_URL="$FIXTURE_URL" \
    PM_DEV_ENROLL_TOKEN="$ENROLL_TOKEN" \
    make prebuild-clean

  WORKSPACE="$(ls -d ios/*.xcworkspace | head -1)"
  APP_SCHEME="$(basename "$WORKSPACE" .xcworkspace)"

  for f in plugins/ui-tests/ios/*.swift; do
    cp "$f" "ios/PuppetMasterUITests/$(basename "$f")"
  done

  BUILD_START=$SECONDS
  PM_UI_TEST_BUILD=1 \
    PM_DEV_CONTROLLER_URL="$FIXTURE_URL" \
    PM_DEV_ENROLL_TOKEN="$ENROLL_TOKEN" \
    xcodebuild build \
      -workspace "$WORKSPACE" -scheme "$APP_SCHEME" \
      -configuration Release -destination "id=$UDID" \
      -derivedDataPath ios/build 2>&1 | tail -3

  PM_UI_TEST_BUILD=1 \
    PM_DEV_CONTROLLER_URL="$FIXTURE_URL" \
    PM_DEV_ENROLL_TOKEN="$ENROLL_TOKEN" \
    xcodebuild build-for-testing \
      -workspace "$WORKSPACE" -scheme PuppetMasterUITests \
      -configuration Release -destination "id=$UDID" \
      -derivedDataPath ios/build 2>&1 | tail -3
  echo "build $((SECONDS - BUILD_START))s"

  xcrun simctl install "$UDID" "ios/build/Build/Products/Release-iphonesimulator/$APP_SCHEME.app"

  python3 -c "
import json
json.dump({
  'udid': '$UDID',
  'fixture_root': '$FIXTURE_ROOT',
  'fixture_url': '$FIXTURE_URL',
  'workspace': '$WORKSPACE',
  'app_scheme': '$APP_SCHEME',
}, open('$ENV_DIR/state.json', 'w'), indent=2)
"
  echo "bootstrap complete"
else
  STATE="$ENV_DIR/state.json"
  UDID="$(python3 -c 'import json; print(json.load(open("'"$STATE"'"))["udid"])')"
  FIXTURE_ROOT="$(python3 -c 'import json; print(json.load(open("'"$STATE"'"))["fixture_root"])')"
  FIXTURE_URL="$(python3 -c 'import json; print(json.load(open("'"$STATE"'"))["fixture_url"])')"
  WORKSPACE="$(python3 -c 'import json; print(json.load(open("'"$STATE"'"))["workspace"])')"

  xcrun simctl boot "$UDID" 2>/dev/null || true

  if ! node "$REPO_ROOT/scripts/controller-fixture.mjs" status --root "$FIXTURE_ROOT" >/dev/null 2>&1; then
    die "fixture at $FIXTURE_ROOT is dead. Run with --clean."
  fi

  REBUILD_TESTS=false
  for f in plugins/ui-tests/ios/*.swift; do
    dst="ios/PuppetMasterUITests/$(basename "$f")"
    if ! cmp -s "$f" "$dst" 2>/dev/null; then
      cp "$f" "$dst"
      REBUILD_TESTS=true
    fi
  done

  if [[ "$REBUILD_TESTS" == true ]]; then
    BUILD_START=$SECONDS
    xcodebuild build-for-testing \
      -workspace "$WORKSPACE" -scheme PuppetMasterUITests \
      -configuration Release -destination "id=$UDID" \
      -derivedDataPath ios/build 2>&1 | tail -3
    echo "test-runner rebuild $((SECONDS - BUILD_START))s"
  fi

  echo "reusing env  sim=$UDID  fixture=$FIXTURE_URL"
fi

ONLY_FLAG="-only-testing:PuppetMasterUITests"
[[ -z "$ONLY_TESTING" ]] || ONLY_FLAG="-only-testing:PuppetMasterUITests/$ONLY_TESTING"

TEST_START=$SECONDS
xcodebuild test-without-building \
    -workspace "$WORKSPACE" -scheme PuppetMasterUITests \
    -configuration Release -destination "id=$UDID" \
    -derivedDataPath ios/build \
    -maximum-test-execution-time-allowance 60 \
    $ONLY_FLAG \
    2>&1 | tee /tmp/pm-uitest-$$.log
RC=${PIPESTATUS[0]}
TEST_ELAPSED=$(( SECONDS - TEST_START ))

[[ "$RC" -eq 0 ]] || { tail -10 /tmp/pm-uitest-$$.log; die "test failed (exit $RC)"; }
echo "test ${TEST_ELAPSED}s  log /tmp/pm-uitest-$$.log"
