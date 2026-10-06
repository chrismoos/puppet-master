import { execFileSync } from "node:child_process";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { establishBrowserBucket } from "./baselineBuckets";
import { validateLaneBinary } from "./laneContract";
import { startDaemon, stopDaemon } from "./daemonHarness";

export default async function globalSetup(): Promise<() => Promise<void>> {
  const pmBinary = process.env.PM_E2E_PM_BIN;
  if (!pmBinary) throw new Error("PM_E2E_PM_BIN must point to the lane's pm binary");
  const lane = process.env.PM_E2E_LANE;
  if (lane !== "worker" && lane !== "integration" && lane !== "terminal-heavy" && lane !== "performance") {
    throw new Error("PM_E2E_LANE must be worker, integration, terminal-heavy, or performance");
  }
  validateLaneBinary(lane, pmBinary);
  const testAgent = process.env.PM_E2E_TESTAGENT_BIN ?? join(dirname(pmBinary), "pm-testagent");
  const root = await mkdtemp(join(tmpdir(), "pm-browser-e2e-"));
  let harness: Awaited<ReturnType<typeof startDaemon>> | undefined;

  try {
    harness = await startDaemon(root, pmBinary, testAgent);
    const cli = (args: string[]) => execFileSync(pmBinary, args, {
      env: { ...harness!.env, PM_SOCKET: harness!.socket },
      encoding: "utf8",
    });
    const bucketId = establishBrowserBucket(cli);
    cli(["project", "add", "--bucket", String(bucketId), "browser-e2e project with a deliberately long identifying label", root]);
    for (let index = 0; index < 75; index += 1) {
      cli(["items", "add", "--bucket", String(bucketId), "--project", "1", "--status", "planned", "--priority", "low", "--body", `Representative high-volume board item ${index}`, `Volume item ${String(index).padStart(2, "0")}`]);
    }
    await stopDaemon(harness.child);
    harness = undefined;
    process.env.PM_E2E_BASELINE_DIR = root;
  } catch (error) {
    if (harness) await stopDaemon(harness.child);
    await rm(root, { recursive: true, force: true });
    throw error;
  }

  return async () => {
    await rm(root, { recursive: true, force: true });
  };
}
