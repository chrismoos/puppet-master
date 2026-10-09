import { spawn, type ChildProcess } from "node:child_process";
import { mkdir, rm, symlink } from "node:fs/promises";
import { createServer } from "node:net";
import { join } from "node:path";

const DAEMON_START_TIMEOUT_MS = 15_000;
const DAEMON_STOP_TIMEOUT_MS = 10_000;
const DAEMON_BIND_ATTEMPTS = 5;
const DAEMON_BIND_RETRY_MS = 250;
const ADDRESS_IN_USE = "Address already in use";

export interface DaemonHarness {
  baseUrl: string;
  /** Where hosts reach the controller, which is not the web address. */
  workerUrl: string;
  child: ChildProcess;
  env: NodeJS.ProcessEnv;
  logs: () => string;
  socket: string;
}

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const address = server.address();
      if (!address || typeof address === "string") {
        server.close();
        reject(new Error("failed to allocate browser test port"));
        return;
      }
      server.close((error) => error ? reject(error) : resolve(address.port));
    });
  });
}

function waitForExit(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  return new Promise((resolve) => child.once("exit", () => resolve()));
}

export async function stopDaemon(child: ChildProcess, options: { disposable?: boolean } = {}): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return;
  if (options.disposable) {
    child.kill("SIGKILL");
    await waitForExit(child);
    return;
  }
  child.kill("SIGINT");
  const stopped = await Promise.race([
    waitForExit(child).then(() => true),
    new Promise<false>((resolve) => setTimeout(() => resolve(false), DAEMON_STOP_TIMEOUT_MS)),
  ]);
  if (!stopped) {
    child.kill("SIGKILL");
    await waitForExit(child);
  }
}

async function waitForDaemon(harness: DaemonHarness): Promise<void> {
  const deadline = Date.now() + DAEMON_START_TIMEOUT_MS;
  while (Date.now() < deadline) {
    if (harness.child.exitCode !== null || harness.child.signalCode !== null) {
      throw new Error(`browser test daemon exited during startup\n${harness.logs()}`);
    }
    try {
      const response = await fetch(`${harness.baseUrl}/api/version`);
      if (response.ok) return;
    } catch {}
    await new Promise((resolve) => setTimeout(resolve, 50));
  }
  throw new Error(`browser test daemon did not start\n${harness.logs()}`);
}

export async function startDaemon(
  root: string,
  pmBinary: string,
  testAgent: string,
  options: { port?: number; workerPort?: number; publicUrl?: boolean | string } = {},
): Promise<DaemonHarness> {
  const binDir = join(root, "bin");
  await mkdir(binDir, { recursive: true });
  const codexShim = join(binDir, "codex");
  await rm(codexShim, { force: true });
  await symlink(testAgent, codexShim);
  // A parallel lane or an outgoing connection can take a freePort port before the daemon binds it.
  for (let attempt = 1; ; attempt += 1) {
    const port = options.port ?? await freePort();
    const workerPort = options.workerPort ?? await freePort();
    const harness = launchDaemon(root, pmBinary, binDir, port, workerPort, options.publicUrl);
    try {
      await waitForDaemon(harness);
      return harness;
    } catch (error) {
      if (attempt >= DAEMON_BIND_ATTEMPTS || !harness.logs().includes(ADDRESS_IN_USE)) throw error;
      await stopDaemon(harness.child, { disposable: true });
      await new Promise((resolve) => setTimeout(resolve, DAEMON_BIND_RETRY_MS));
    }
  }
}

function launchDaemon(
  root: string,
  pmBinary: string,
  binDir: string,
  port: number,
  workerPort: number,
  publicUrl: boolean | string | undefined,
): DaemonHarness {
  const baseUrl = `http://127.0.0.1:${port}`;
  const workerUrl = `wss://127.0.0.1:${workerPort}`;
  const socket = join(root, "pm.sock");
  const env = {
    ...process.env,
    PATH: `${binDir}:${process.env.PATH ?? ""}`,
    SHELL: "/bin/sh",
    TERM: "xterm-256color",
    // Spawning a Codex session records the project directory as trusted
    // in the Codex home. Lane daemons run in parallel over the same
    // project, so they answer into their own home rather than appending
    // to the developer's real one at once.
    CODEX_HOME: join(root, "codex"),
  };
  const child = spawn(pmBinary, [
    "daemon",
    "--db", join(root, "pm.db"),
    "--scrollback-dir", join(root, "scrollback"),
    "--socket", socket,
    "--http", `127.0.0.1:${port}`,
    ...(publicUrl
      ? ["--public-url", typeof publicUrl === "string" ? publicUrl : baseUrl]
      : []),
    // The worker plane's default port is fixed, so lane daemons running
    // side by side would fight over it.
    "--worker-listen", `127.0.0.1:${workerPort}`,
  ], { env, stdio: ["ignore", "pipe", "pipe"] });
  let output = "";
  const append = (chunk: Buffer) => {
    output = `${output}${chunk.toString()}`.slice(-32_768);
  };
  child.stdout?.on("data", append);
  child.stderr?.on("data", append);
  return { baseUrl, workerUrl, child, env, logs: () => output, socket };
}
