// Drives the fixture against a real controller, because the enrollment
// contract it advertises is the daemon's behavior and not something a
// stub can answer for. Needs a built `pm` and `pm-testagent`, which is
// why `make controller-fixture-live-test` builds them first.

import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";

import { ENROLL_TOKEN_MIN_REMAINING_MS, enrollTokenHash } from "./controller-fixture.mjs";

const fixtureScript = fileURLToPath(new URL("./controller-fixture.mjs", import.meta.url));

let fixture = null;

/// Runs the command line and holds it to the stdout contract: exactly one
/// JSON document, whatever else the run had to say.
function fixtureCommand(args, expected = 0) {
  const result = spawnSync(process.execPath, [fixtureScript, ...args], { encoding: "utf8" });
  assert.equal(result.status, expected, `${args.join(" ")} failed: ${result.stderr}`);
  return JSON.parse(result.stdout);
}

function enrollmentTokens() {
  const database = new DatabaseSync(fixture.db, { readOnly: true });
  try {
    return database.prepare(
      "SELECT token_hash, created_at_unix_ms, expires_at_unix_ms, used_at_unix_ms FROM mobile_enrollment_tokens",
    ).all();
  } finally {
    database.close();
  }
}

async function enroll(token, deviceId) {
  const response = await fetch(`${fixture.baseUrl}/api/mobile/devices/enroll`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ deviceId, name: deviceId, platform: "test", enrollToken: token }),
  });
  return { status: response.status, body: await response.json() };
}

before(() => {
  fixture = fixtureCommand(["start"]);
});

after(() => {
  if (fixture) fixtureCommand(["stop", "--root", fixture.root]);
});

test("the advertised enrollment token is unused and long-lived enough to hand to a client", () => {
  const rows = enrollmentTokens();
  const advertised = rows.find((row) => row.token_hash === enrollTokenHash(fixture.auth.mobileEnrollToken));
  assert.ok(advertised, "the advertised token is not in the controller's store");
  assert.equal(advertised.used_at_unix_ms, null, "the advertised token was already spent");
  assert.equal(advertised.expires_at_unix_ms, fixture.auth.mobileEnrollTokenExpiresAtUnixMs);
  const lifetimeMinutes = Math.round((advertised.expires_at_unix_ms - advertised.created_at_unix_ms) / 60_000);
  // The fixture's own configured lifetime, not the daemon's ten-minute
  // default, which is what an iOS build round outlives.
  assert.equal(lifetimeMinutes, 240);
  assert.ok(advertised.expires_at_unix_ms - Date.now() > ENROLL_TOKEN_MIN_REMAINING_MS);
});

test("seeding spends a token of its own, never the advertised one", () => {
  const rows = enrollmentTokens();
  const spent = rows.filter((row) => row.used_at_unix_ms !== null);
  assert.equal(spent.length, 1, "seeding should spend exactly one enrollment token");
  assert.notEqual(spent[0].token_hash, enrollTokenHash(fixture.auth.mobileEnrollToken));
  // The device that token paid for is the fixture's own, and it is
  // enrolled and usable, so seeding really did complete on its own token.
  assert.equal(typeof fixture.auth.mobileDeviceId, "string");
  assert.ok(fixture.auth.mobileAccessToken.length > 0);
});

test("a re-seed leaves the advertised token untouched", () => {
  const before = enrollmentTokens();
  fixtureCommand(["seed", "--root", fixture.root]);
  const after = enrollmentTokens();
  const advertised = after.find((row) => row.token_hash === enrollTokenHash(fixture.auth.mobileEnrollToken));
  assert.ok(advertised, "re-seeding dropped the advertised token");
  assert.equal(advertised.used_at_unix_ms, null, "re-seeding spent the advertised token");
  assert.equal(after.filter((row) => row.used_at_unix_ms !== null).length,
    before.filter((row) => row.used_at_unix_ms !== null).length);
});

test("the advertised token enrolls exactly one client", async () => {
  const first = await enroll(fixture.auth.mobileEnrollToken, "external-client-one");
  assert.equal(first.status, 200, JSON.stringify(first.body));
  assert.equal(first.body.device.appInstallationId, "external-client-one");
  assert.ok(first.body.tokens.accessToken.length > 0);

  const second = await enroll(fixture.auth.mobileEnrollToken, "external-client-two");
  assert.notEqual(second.status, 200);
  assert.match(JSON.stringify(second.body), /already used/);

  const advertised = enrollmentTokens()
    .find((row) => row.token_hash === enrollTokenHash(fixture.auth.mobileEnrollToken));
  assert.notEqual(advertised.used_at_unix_ms, null);
});

test("enroll-token mints a fresh token for a client that is about to launch", async () => {
  const minted = fixtureCommand(["enroll-token", "--root", fixture.root]);
  assert.notEqual(minted.token, fixture.auth.mobileEnrollToken);
  assert.ok(minted.expiresInSeconds > ENROLL_TOKEN_MIN_REMAINING_MS / 1000);
  const record = enrollmentTokens().find((row) => row.token_hash === enrollTokenHash(minted.token));
  assert.equal(record.used_at_unix_ms, null);

  const enrolled = await enroll(minted.token, "external-client-three");
  assert.equal(enrolled.status, 200, JSON.stringify(enrolled.body));
  const again = await enroll(minted.token, "external-client-four");
  assert.notEqual(again.status, 200);
});

test("the fixture's own seeded state is still intact after the enrollment round", () => {
  const status = fixtureCommand(["status", "--root", fixture.root]);
  assert.equal(status.ready, true);
  assert.equal(status.fixtures.sessions.length, 2);
  assert.equal(status.fixtures.plans.length, 2);
  assert.equal(readFileSync(status.logFile, "utf8").length >= 0, true);
});
