import { execFileSync, spawn, type ChildProcess } from "node:child_process";
import { copyFile, cp, mkdir, mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { test as base } from "@playwright/test";
import { startDaemon, stopDaemon } from "./daemonHarness";

const DATABASE_BUSY_TIMEOUT_MS = 5000;

// The daemon writes to this database too, so a fixture waits out its lock.
function openDaemonDatabase(root: string, options: { readOnly?: boolean } = {}): DatabaseSync {
  const database = new DatabaseSync(join(root, "pm.db"), options);
  database.exec(`PRAGMA busy_timeout = ${DATABASE_BUSY_TIMEOUT_MS}`);
  return database;
}

/** A thread and its conversation, as a review would already hold them.
 * The agent side of a review has no CLI, so a reader's view of answered,
 * sent, and resolved work is staged in the database. */
export type SeedThread = {
  path: string;
  line: number;
  state: "draft" | "sent" | "answered" | "resolved";
  messages: readonly { author: "user" | "session"; body: string }[];
};

type E2eFixtures = {
  isolatedDaemon: {
    setSessionActivity: (title: string, activity: string) => void;
    seedSessionReports: (title: string, reports: Array<{
      tsUnixMs: number;
      kind: "checkpoint" | "status" | "progress" | "blocked";
      payload: Record<string, unknown>;
    }>) => void;
    seedSessionForward: (title: string, label: string, targetPort?: number) => void;
    setSessionParent: (childTitle: string, supervisorTitle: string) => void;
    seedEndedSessions: (count: number, promptBytes?: number) => void;
    seedOfflineHosts: (names: readonly string[]) => void;
    seedSessionOnOfflineHost: (title: string, hostName: string) => void;
    setProjectPath: (path: string) => void;
    seedReviewThreads: (reviewId: number, threads: readonly SeedThread[]) => void;
    agentReply: (sessionTitle: string, threadId: number, body: string) => Promise<void>;
    callAgentTool: (sessionTitle: string, name: string, args: Record<string, unknown>) => Promise<string>;
    setSessionEndedState: (title: string, state: "exited" | "failed", endedAgoMs?: number) => void;
    hookSession: (title: string, kind: "needs-input" | "prompt-submitted", detail?: string) => void;
    explicitCwd: string;
    restart: (options?: { publicUrl?: boolean | string }) => Promise<void>;
    session: (title: string) => { id: number; cwd: string; role: string; supervisorApi: number; workerId: number } | null;
    workerId: (name: string) => number | null;
    startRemoteWorker: (token: string) => Promise<void>;
    workerDefaultRoot: () => string | null;
  };
};

function stopProcess(child: ChildProcess): Promise<void> {
  if (child.exitCode !== null || child.signalCode !== null) return Promise.resolve();
  child.kill("SIGKILL");
  return new Promise((resolve) => child.once("exit", () => resolve()));
}

export const test = base.extend<E2eFixtures>({
  page: async ({ page }, use, testInfo) => {
    await use(page);
    if (testInfo.status === testInfo.expectedStatus) return;
    const diagnostics = await page.evaluate(() => {
      const stage = (window as unknown as { __pmStage?: {
        debugSnapshot(): Array<Record<string, unknown>>;
        layers: Map<string, { term: { buffer: { active: {
          baseY: number;
          length: number;
          getLine(row: number): { translateToString(trimRight?: boolean): string } | undefined;
        } } } }>;
      } }).__pmStage;
      if (!stage) return null;
      const snapshot = stage.debugSnapshot();
      const tails: Record<string, string[]> = {};
      for (const [key, layer] of stage.layers) {
        const buffer = layer.term.buffer.active;
        const lines: string[] = [];
        for (let row = 0; row < buffer.length; row += 1) {
          const text = buffer.getLine(row)?.translateToString(true) ?? "";
          if (text) lines.push(`${row}: ${text}`);
        }
        tails[key] = lines.slice(-40);
      }
      const active = document.activeElement;
      const activeInfo = active
        ? `${active.tagName}.${active.className} in ${active.closest(".term-layer")?.getAttribute("style") ?? "no-term-layer"}`
        : "none";
      return { snapshot, tails, url: location.href, activeInfo };
    }).catch((error) => ({ evaluateError: String(error) }));
    await testInfo.attach("pm-terminal-diagnostics", {
      body: JSON.stringify(diagnostics, null, 2),
      contentType: "application/json",
    });
  },
  isolatedDaemon: [async ({}, use) => {
    const baseline = process.env.PM_E2E_BASELINE_DIR;
    const pmBinary = process.env.PM_E2E_PM_BIN;
    const testAgent = process.env.PM_E2E_TESTAGENT_BIN;
    if (!baseline || !pmBinary || !testAgent) {
      throw new Error("isolated browser tests require PM_E2E_BASELINE_DIR and lane binaries");
    }

    const root = await mkdtemp(join(tmpdir(), "pm-browser-test-"));
    const previousBaseUrl = process.env.PM_E2E_BASE_URL;
    const previousSocket = process.env.PM_E2E_SOCKET;
    let harness: Awaited<ReturnType<typeof startDaemon>> | undefined;
    let remoteWorker: ChildProcess | undefined;
    try {
      await copyFile(join(baseline, "pm.db"), join(root, "pm.db"));
      await cp(join(baseline, "scrollback"), join(root, "scrollback"), { recursive: true });
      harness = await startDaemon(root, pmBinary, testAgent);
      const cli = (args: string[]) => execFileSync(pmBinary, args, {
        env: { ...harness!.env, PM_SOCKET: harness!.socket },
        encoding: "utf8",
      });
      cli(["spawn", "--project", "1", "--agent", "codex", "--title", "browser-e2e"]);
      cli(["spawn", "--project", "1", "--agent", "codex", "--title", "browser-e2e-two"]);
      process.env.PM_E2E_BASE_URL = harness.baseUrl;
      process.env.PM_E2E_SOCKET = harness.socket;
      const explicitCwd = join(root, "explicit-remote-cwd");
      await mkdir(explicitCwd);
      await use({
        explicitCwd,
        restart: async (options = {}) => {
          if (!harness) throw new Error("daemon is not running");
          const port = Number(new URL(harness.baseUrl).port);
          // A remote worker reconnects to the address it enrolled against.
          const workerPort = Number(new URL(harness.workerUrl).port);
          await stopDaemon(harness.child);
          harness = await startDaemon(root, pmBinary, testAgent, { port, workerPort, publicUrl: options.publicUrl });
          process.env.PM_E2E_BASE_URL = harness.baseUrl;
          process.env.PM_E2E_SOCKET = harness.socket;
        },
        workerId: (name) => {
          const database = openDaemonDatabase(root, { readOnly: true });
          try {
            const row = database.prepare("SELECT id FROM workers WHERE name = ?").get(name) as { id: number } | undefined;
            return row ? Number(row.id) : null;
          } finally {
            database.close();
          }
        },
        session: (title) => {
          const database = openDaemonDatabase(root, { readOnly: true });
          try {
            const row = database.prepare(
              "SELECT id, cwd, role, supervisor_api, worker_id FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(title) as { id: number; cwd: string; role: string; supervisor_api: number; worker_id: number } | undefined;
            return row ? {
              id: row.id,
              cwd: row.cwd,
              role: row.role,
              supervisorApi: row.supervisor_api,
              workerId: row.worker_id,
            } : null;
          } finally {
            database.close();
          }
        },
        startRemoteWorker: async (token) => {
          if (!harness) throw new Error("daemon is not running");
          const workerConfig = join(root, "remote-worker-config");
          const workerData = join(root, "remote-worker-data");
          await mkdir(workerConfig);
          await mkdir(workerData);
          remoteWorker = spawn(pmBinary, [
            "worker",
            "--controller", harness.workerUrl,
            "--token", token,
          ], {
            env: {
              ...harness.env,
              HOME: "~/",
              XDG_CONFIG_HOME: workerConfig,
              XDG_DATA_HOME: workerData,
            },
            stdio: ["ignore", "pipe", "pipe"],
          });
          remoteWorker.stdout?.resume();
          remoteWorker.stderr?.resume();
          await new Promise((resolve) => setTimeout(resolve, 100));
          if (remoteWorker.exitCode !== null) {
            throw new Error(`remote worker exited with ${remoteWorker.exitCode}`);
          }
        },
        // A host that has enrolled and never connected is exactly what the
        // sidebar renders as offline, so the row is seeded rather than staged
        // by disconnecting a live worker.
        // A path that has gone missing is ordinary config, not corruption,
        // so it is written the way a stale configuration would leave it.
        setProjectPath: (path) => {
          const database = openDaemonDatabase(root);
          try {
            database.prepare("UPDATE projects SET path = ? WHERE id = 1").run(path);
          } finally {
            database.close();
          }
        },
        seedOfflineHosts: (names) => {
          const database = openDaemonDatabase(root);
          try {
            const insert = database.prepare(
              "INSERT INTO workers (name, hostname, created_at_unix_ms) VALUES (?, ?, ?)",
            );
            database.exec("BEGIN IMMEDIATE");
            for (const name of names) insert.run(name, `${name}.local`, Date.now());
            database.exec("COMMIT");
          } catch (error) {
            database.exec("ROLLBACK");
            throw error;
          } finally {
            database.close();
          }
        },
        // Re-homes a baseline session onto a host that never connects, keeping
        // the daemon's intent to run it. That is what a remote session looks
        // like while its host is away.
        seedSessionOnOfflineHost: (title, hostName) => {
          const database = openDaemonDatabase(root);
          try {
            database.exec("BEGIN IMMEDIATE");
            database
              .prepare("INSERT INTO workers (name, hostname, created_at_unix_ms) VALUES (?, ?, ?)")
              .run(hostName, `${hostName}.local`, Date.now());
            const workerId = Number(
              (database.prepare("SELECT id FROM workers WHERE name = ?").get(hostName) as { id: number }).id,
            );
            const moved = database
              .prepare("UPDATE sessions SET worker_id = ?, desired_running = 1 WHERE task_title = ?")
              .run(workerId, title);
            if (Number(moved.changes) !== 1) throw new Error(`expected one session named ${title}`);
            database.exec("COMMIT");
          } catch (error) {
            database.exec("ROLLBACK");
            throw error;
          } finally {
            database.close();
          }
        },
        seedReviewThreads: (reviewId, threads) => {
          const database = openDaemonDatabase(root);
          try {
            const thread = database.prepare(
              "INSERT INTO review_threads (review_id, path, line, side, excerpt, state, created_at_unix_ms) \
               VALUES (?, ?, ?, 'right', '', ?, ?)",
            );
            const message = database.prepare(
              "INSERT INTO review_messages (thread_id, author, body, created_at_unix_ms) VALUES (?, ?, ?, ?)",
            );
            database.exec("BEGIN IMMEDIATE");
            for (const seed of threads) {
              const now = Date.now();
              const id = thread.run(reviewId, seed.path, seed.line, seed.state, now)
                .lastInsertRowid as number;
              for (const m of seed.messages) message.run(id, m.author, m.body, now);
            }
            database.exec("COMMIT");
          } catch (error) {
            database.exec("ROLLBACK");
            throw error;
          } finally {
            database.close();
          }
        },
        // A round the agent produced, taken the way the agent takes it:
        // the MCP endpoint records the tree it left behind as that
        // round's revision, which staging the message alone would not.
        agentReply: async (sessionTitle, threadId, body) => {
          if (!harness) throw new Error("daemon is not running");
          const database = openDaemonDatabase(root, { readOnly: true });
          let token: string;
          try {
            const row = database.prepare(
              "SELECT session_token FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(sessionTitle) as { session_token: string } | undefined;
            if (!row?.session_token) throw new Error(`no session token for ${sessionTitle}`);
            token = row.session_token;
          } finally {
            database.close();
          }
          const response = await fetch(`${harness.baseUrl}/mcp`, {
            method: "POST",
            headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
            body: JSON.stringify({
              jsonrpc: "2.0",
              id: 1,
              method: "tools/call",
              params: {
                name: "post_review_reply",
                arguments: { thread_id: threadId, body, addressed: true },
              },
            }),
          });
          const answer = await response.json() as { error?: { message: string }; result?: { isError?: boolean; content?: Array<{ text?: string }> } };
          if (answer.error || answer.result?.isError) {
            throw new Error(`agent reply failed: ${JSON.stringify(answer)}`);
          }
        },
        callAgentTool: async (sessionTitle, name, args) => {
          if (!harness) throw new Error("daemon is not running");
          const database = openDaemonDatabase(root, { readOnly: true });
          let token: string;
          try {
            const row = database.prepare(
              "SELECT session_token FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(sessionTitle) as { session_token: string } | undefined;
            if (!row?.session_token) throw new Error(`no session token for ${sessionTitle}`);
            token = row.session_token;
          } finally {
            database.close();
          }
          const response = await fetch(`${harness.baseUrl}/mcp`, {
            method: "POST",
            headers: { authorization: `Bearer ${token}`, "content-type": "application/json" },
            body: JSON.stringify({
              jsonrpc: "2.0",
              id: 1,
              method: "tools/call",
              params: { name, arguments: args },
            }),
          });
          const answer = await response.json() as { error?: { message: string }; result?: { isError?: boolean; content?: Array<{ text?: string }> } };
          if (answer.error || answer.result?.isError) {
            throw new Error(`agent tool call failed: ${JSON.stringify(answer)}`);
          }
          return answer.result?.content?.[0]?.text ?? "";
        },
        workerDefaultRoot: () => {
          const database = openDaemonDatabase(root, { readOnly: true });
          try {
            const row = database.prepare(
              "SELECT default_project_root FROM workers WHERE id != 0 ORDER BY id DESC LIMIT 1",
            ).get() as { default_project_root: string } | undefined;
            return row?.default_project_root ?? null;
          } finally {
            database.close();
          }
        },
        setSessionActivity: (title, activity) => {
          const database = openDaemonDatabase(root);
          try {
            const result = database.prepare(
              "UPDATE sessions SET activity = ? WHERE task_title = ?",
            ).run(activity, title);
            if (Number(result.changes) !== 1) {
              throw new Error(`expected one session named ${title}, updated ${result.changes}`);
            }
          } finally {
            database.close();
          }
        },
        seedSessionReports: (title, reports) => {
          const database = openDaemonDatabase(root);
          try {
            const session = database.prepare(
              "SELECT id FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(title) as { id: number } | undefined;
            if (!session) throw new Error(`expected one session named ${title}`);
            const insert = database.prepare(
              "INSERT INTO activity_reports (session_id, ts_unix_ms, kind, payload) VALUES (?, ?, ?, ?)",
            );
            database.exec("BEGIN IMMEDIATE");
            for (const report of reports) {
              insert.run(session.id, report.tsUnixMs, report.kind, JSON.stringify(report.payload));
            }
            database.exec("COMMIT");
          } catch (error) {
            database.exec("ROLLBACK");
            throw error;
          } finally {
            database.close();
          }
        },
        seedSessionForward: (title, label, targetPort = 4173) => {
          const database = openDaemonDatabase(root);
          try {
            const session = database.prepare(
              "SELECT id FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(title) as { id: number } | undefined;
            if (!session) throw new Error(`expected one session named ${title}`);
            database.prepare(`
              INSERT INTO session_forwards
                (session_id, worker_port, listener_port, label, scheme, created_at_unix_ms)
              VALUES (?, ?, 0, ?, 'http', ?)
            `).run(session.id, targetPort, label, Date.now());
          } finally {
            database.close();
          }
        },
        // Only the MCP spawn path records a parent, so a browser test seeds the
        // link the way the daemon would have written it.
        setSessionParent: (childTitle, supervisorTitle) => {
          const database = openDaemonDatabase(root);
          try {
            const parent = database.prepare(
              "SELECT id FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(supervisorTitle) as { id: number } | undefined;
            if (!parent) throw new Error(`expected one session named ${supervisorTitle}`);
            const result = database.prepare(
              "UPDATE sessions SET spawned_by_session_id = ? WHERE task_title = ?",
            ).run(parent.id, childTitle);
            if (Number(result.changes) !== 1) {
              throw new Error(`expected one session named ${childTitle}, updated ${result.changes}`);
            }
          } finally {
            database.close();
          }
        },
        seedEndedSessions: (count, promptBytes = 0) => {
          const database = openDaemonDatabase(root);
          try {
            database.exec("BEGIN IMMEDIATE");
            const insert = database.prepare(`
              INSERT INTO sessions(project_id,agent,state,task_title,task_prompt,agent_session_id,
                created_at_unix_ms,ended_at_unix_ms,exit_code,state_detail,permission_mode,worker_id,
                cwd,desired_running,headline,summary,items_api,supervisor_api,role)
              VALUES(1,'test','exited',?,?,?,?,?,0,'','default',0,'/tmp',0,?,?,1,0,'worker')
            `);
            const prompt = "P".repeat(promptBytes);
            const base = Date.now() - 120_000;
            for (let i = 0; i < count; i += 1) {
              insert.run(
                `archived-session-${String(i).padStart(3, "0")}`,
                prompt,
                `archive-conversation-${i}`,
                base - i - 1_000,
                base - i,
                i === count - 1 ? "old searchable needle" : "",
                i === count - 1 ? "metadata only" : "",
              );
            }
            database.exec("COMMIT");
          } catch (error) {
            database.exec("ROLLBACK");
            throw error;
          } finally {
            database.close();
          }
        },
        setSessionEndedState: (title, state, endedAgoMs = 0) => {
          const database = openDaemonDatabase(root);
          try {
            const result = database.prepare(
              "UPDATE sessions SET state=?, ended_at_unix_ms=?, desired_running=0 WHERE task_title=?",
            ).run(state, Date.now() - endedAgoMs, title);
            if (Number(result.changes) !== 1) throw new Error(`expected one session named ${title}`);
          } finally {
            database.close();
          }
        },
        hookSession: (title, kind, detail = "") => {
          const database = openDaemonDatabase(root, { readOnly: true });
          try {
            const row = database.prepare(
              "SELECT session_token FROM sessions WHERE task_title = ? ORDER BY id DESC LIMIT 1",
            ).get(title) as { session_token: string } | undefined;
            if (!row?.session_token) throw new Error(`expected hook token for session ${title}`);
            execFileSync(pmBinary, ["_hook", kind], {
              env: { ...harness!.env, PM_SOCKET: harness!.socket, PM_SESSION_TOKEN: row.session_token },
              input: JSON.stringify({ message: detail }),
              encoding: "utf8",
            });
          } finally {
            database.close();
          }
        },
      });
    } finally {
      if (remoteWorker) await stopProcess(remoteWorker);
      if (harness) await stopDaemon(harness.child, { disposable: true });
      if (previousBaseUrl === undefined) delete process.env.PM_E2E_BASE_URL;
      else process.env.PM_E2E_BASE_URL = previousBaseUrl;
      if (previousSocket === undefined) delete process.env.PM_E2E_SOCKET;
      else process.env.PM_E2E_SOCKET = previousSocket;
      await rm(root, { recursive: true, force: true });
    }
  }, { auto: true }],
});

export { expect } from "@playwright/test";
export type { BrowserContext, Locator, Page } from "@playwright/test";
