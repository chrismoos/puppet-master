import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// Measurement harness for "marking a file viewed stalls the review".
// It prints numbers rather than asserting a contract.
//
// A config is "<files>x<lines>" for a review of uniform files, or
// "<files>x<lines>+tiny" to append one 8-line file and mark that one,
// which separates the cost of the marked file from the cost of the
// review it sits in.

const CONFIGS = (process.env.PM_MEASURE_CONFIGS
  ?? "4x120,16x120,32x120,64x120,16x480,16x1920,32x480+tiny,4x120+tiny")
  .split(",")
  .map((label) => {
    const [shape, tiny] = label.split("+");
    const [files, lines] = shape.split("x").map(Number);
    return { files, lines, tiny: tiny === "tiny", label };
  });
const REPEATS = Number(process.env.PM_MEASURE_REPEATS ?? "5");
const OUT = process.env.PM_MEASURE_OUT;

/** Sorts ahead of the generated files so one still follows it, which
 * is what marking it viewed scrolls to. */
const TINY_NAME = "a-tiny.ts";
const TINY_LINES = 8;
const APPROX_BYTES_PER_LINE = 60;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function fileName(index: number): string {
  return `file${String(index).padStart(3, "0")}.ts`;
}

function seedWorktree(fileCount: number, lines: number, tiny: boolean): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-measure-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  const body = (name: string, count: number, edited: boolean) =>
    Array.from({ length: count }, (_, i) => {
      const value = edited && i % 4 === 0 ? i + 1000 : i;
      return `export const ${name.replace(/\W/g, "_")}_${i} = { id: ${value}, name: "row ${i}" };`;
    }).join("\n") + "\n";
  const plan = Array.from({ length: fileCount }, (_, i) => ({ name: fileName(i), lines }));
  if (tiny) plan.unshift({ name: TINY_NAME, lines: TINY_LINES });

  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  for (const file of plan) writeFileSync(join(dir, file.name), body(file.name, file.lines, false));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  for (const file of plan) writeFileSync(join(dir, file.name), body(file.name, file.lines, true));
  return dir;
}

function openReview(sessionId: number, worktree: string, label: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", label]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

type Sample = {
  collapseMs: number;
  settleMs: number;
  quietMs: number;
  frames: number;
  longTaskMs: number;
  longestTaskMs: number;
  diffFetches: number;
  detailFetches: number;
};

/** How long the page must go without blocking the main thread before the
 * click counts as finished. Work the click starts can land after the next
 * file is on screen, and that still reads as a stall. */
const QUIET_MS = 700;

/** Putting a 30,000-row review back on screen takes longer than the
 * default expect timeout, and that wait is setup rather than measurement. */
const SLOW_RENDER_MS = 120_000;

/** Clicks the file's Viewed box and waits for the review to answer: the
 * marked file's body gone, and the file after it parked at the top and
 * holding still. Both are measured in the page so the numbers are the
 * reader's, not the test runner's. */
async function measureOnce(page: Page, markedId: string, targetId: string): Promise<Sample> {
  return page.evaluate(
    async ({ marked, target, quiet }) => {
      const w = window as unknown as {
        __pmLongTasks?: { duration: number; end: number }[];
        __pmFetches?: string[];
      };
      w.__pmLongTasks = [];
      w.__pmFetches = [];
      const observer = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) {
          w.__pmLongTasks!.push({ duration: entry.duration, end: entry.startTime + entry.duration });
        }
      });
      observer.observe({ entryTypes: ["longtask"] });
      const resources = new PerformanceObserver((list) => {
        for (const entry of list.getEntries()) w.__pmFetches!.push(entry.name);
      });
      resources.observe({ entryTypes: ["resource"] });

      const box = document.querySelector(".review-diff") as HTMLElement;
      const offset = () => {
        const head = document.querySelector(`#${target} .review-file-head`);
        if (!head) return Number.NaN;
        return head.getBoundingClientRect().top - box.getBoundingClientRect().top;
      };
      const boxes = document.querySelectorAll<HTMLInputElement>(
        `#${marked} .review-file-head input[type="checkbox"]`,
      );
      const checkbox = boxes[boxes.length - 1];
      const frame = () => new Promise<void>((r) => requestAnimationFrame(() => r()));

      const start = performance.now();
      checkbox.click();
      let collapseMs = Number.NaN;
      let settleMs = Number.NaN;
      let frames = 0;
      let stable = 0;
      let lastBusy = start;
      for (let i = 0; i < 6000; i += 1) {
        await frame();
        frames += 1;
        const now = performance.now();
        if (Number.isNaN(collapseMs) && !document.querySelector(`#${marked} .review-rows`)) {
          collapseMs = now - start;
        }
        if (Number.isNaN(settleMs) && !Number.isNaN(collapseMs)) {
          if (Math.abs(offset()) <= 2) {
            stable += 1;
            if (stable >= 3) settleMs = now - start;
          } else {
            stable = 0;
          }
        }
        for (const task of w.__pmLongTasks!) lastBusy = Math.max(lastBusy, task.end);
        if (!Number.isNaN(settleMs) && now - lastBusy >= quiet) break;
        if (now - start > 40_000) break;
      }
      observer.disconnect();
      resources.disconnect();
      const tasks = w.__pmLongTasks!;
      return {
        collapseMs,
        settleMs,
        quietMs: lastBusy - start,
        frames,
        longTaskMs: tasks.reduce((sum, t) => sum + t.duration, 0),
        longestTaskMs: tasks.reduce((max, t) => Math.max(max, t.duration), 0),
        diffFetches: w.__pmFetches!.filter((u) => u.includes("/diff?")).length,
        detailFetches: w.__pmFetches!.filter((u) => /\/api\/reviews\/\d+\?|\/api\/reviews\/\d+$/.test(u)).length,
      };
    },
    { marked: markedId, target: targetId, quiet: QUIET_MS },
  );
}

function cssId(path: string): string {
  return path.replace(/[^a-zA-Z0-9_-]/g, "_");
}

function median(values: number[]): number {
  const sorted = [...values].sort((a, b) => a - b);
  return sorted[Math.floor(sorted.length / 2)];
}

// Trace recording walks the whole DOM on every mutation, which on a
// 30,000-row review costs more than the click being measured. It is the
// harness, not the page, so it has no place in these numbers.
test.use({ trace: "off" });

test("measure the cost of marking a file viewed", async ({ page, isolatedDaemon }) => {
  test.setTimeout(1_800_000);
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  await logIn(page);

  const report: Record<string, unknown>[] = [];
  for (const config of CONFIGS) {
    const reviewId = openReview(
      session!.id,
      seedWorktree(config.files, config.lines, config.tiny),
      `measure ${config.label}`,
    );
    const cdp = await page.context().newCDPSession(page);
    await cdp.send("Performance.enable");

    const samples: Sample[] = [];
    const metricRuns: Record<string, number>[] = [];
    for (let run = 0; run < REPEATS; run += 1) {
      // Each repeat is the same operation from the same state: the page
      // is reloaded so an accumulating set of viewed files does not make
      // later repeats cheaper than earlier ones.
      await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
      await expect(page.locator(".review-rail-file"))
        .toHaveCount(config.files + (config.tiny ? 1 : 0), { timeout: SLOW_RENDER_MS });
      await expect(page.locator(".review-file").first().locator(".review-rows"))
        .toBeVisible({ timeout: SLOW_RENDER_MS });

      const marked = config.tiny ? TINY_NAME : fileName(0);
      const target = config.tiny ? fileName(0) : fileName(1);
      const before = await cdp.send("Performance.getMetrics");
      const sample = await measureOnce(page, `review-file-${cssId(marked)}`, `review-file-${cssId(target)}`);
      const after = await cdp.send("Performance.getMetrics");
      const delta: Record<string, number> = {};
      for (const metric of after.metrics) {
        const started = before.metrics.find((m) => m.name === metric.name)?.value ?? 0;
        if (/Duration$/.test(metric.name)) delta[metric.name] = (metric.value - started) * 1000;
      }
      samples.push(sample);
      metricRuns.push(delta);

      // Put the file back so the next repeat starts where this one did.
      await page.locator(`#review-file-${cssId(marked)}`)
        .getByRole("checkbox", { name: "Viewed" }).click();
      await expect(page.locator(`#review-file-${cssId(marked)} .review-rows`))
        .toBeVisible({ timeout: SLOW_RENDER_MS });
    }

    const lines = config.files * config.lines + (config.tiny ? TINY_LINES : 0);
    report.push({
      config: config.label,
      files: config.files + (config.tiny ? 1 : 0),
      markedFileLines: config.tiny ? TINY_LINES : config.lines,
      reviewLines: lines,
      approxReviewKiB: Math.round((lines * APPROX_BYTES_PER_LINE) / 1024),
      collapseMsMedian: Math.round(median(samples.map((s) => s.collapseMs))),
      settleMsMedian: Math.round(median(samples.map((s) => s.settleMs))),
      quietMsMedian: Math.round(median(samples.map((s) => s.quietMs))),
      longestTaskMsMedian: Math.round(median(samples.map((s) => s.longestTaskMs))),
      longTaskMsMedian: Math.round(median(samples.map((s) => s.longTaskMs))),
      framesMedian: median(samples.map((s) => s.frames)),
      scriptMsMedian: Math.round(median(metricRuns.map((m) => m.ScriptDuration ?? 0))),
      layoutMsMedian: Math.round(median(metricRuns.map((m) => m.LayoutDuration ?? 0))),
      styleMsMedian: Math.round(median(metricRuns.map((m) => m.RecalcStyleDuration ?? 0))),
      taskMsMedian: Math.round(median(metricRuns.map((m) => m.TaskDuration ?? 0))),
      diffFetchesMedian: median(samples.map((s) => s.diffFetches)),
      detailFetchesMedian: median(samples.map((s) => s.detailFetches)),
      settleRaw: samples.map((s) => Math.round(s.settleMs)),
      quietRaw: samples.map((s) => Math.round(s.quietMs)),
    });
    await cdp.detach();
    process.stdout.write(`MEASURE ${JSON.stringify(report[report.length - 1])}\n`);
    if (OUT) writeFileSync(OUT, JSON.stringify(report, null, 1));
  }
});

test("profile the click that marks a file viewed", async ({ page, isolatedDaemon }) => {
  test.setTimeout(600_000);
  const [files, lines] = (process.env.PM_PROFILE_CONFIG ?? "16x480").split("x").map(Number);
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  await logIn(page);
  const reviewId = openReview(session!.id, seedWorktree(files, lines, false), "profile");

  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file")).toHaveCount(files, { timeout: SLOW_RENDER_MS });
  await expect(page.locator(".review-file").first().locator(".review-rows"))
    .toBeVisible({ timeout: SLOW_RENDER_MS });

  const cdp = await page.context().newCDPSession(page);
  await cdp.send("Profiler.enable");
  await cdp.send("Profiler.setSamplingInterval", { interval: 100 });
  await cdp.send("Profiler.start");
  await measureOnce(page, `review-file-${cssId(fileName(0))}`, `review-file-${cssId(fileName(1))}`);
  const { profile } = await cdp.send("Profiler.stop");

  const selfMs = new Map<string, number>();
  const totalMs = new Map<string, number>();
  const byId = new Map<number, (typeof profile.nodes)[number]>();
  for (const node of profile.nodes) byId.set(node.id, node);
  const hits = profile.nodes.reduce((sum, node) => sum + (node.hitCount ?? 0), 0);
  const msPerHit = (profile.endTime - profile.startTime) / 1000 / Math.max(1, hits);
  const label = (node: (typeof profile.nodes)[number]) => {
    const frame = node.callFrame;
    return `${frame.functionName || "(anonymous)"} @ ${frame.url.split("/").pop()}:${frame.lineNumber + 1}`;
  };
  const subtreeHits = (id: number, seen = new Set<number>()): number => {
    if (seen.has(id)) return 0;
    seen.add(id);
    const node = byId.get(id);
    if (!node) return 0;
    let sum = node.hitCount ?? 0;
    for (const child of node.children ?? []) sum += subtreeHits(child, seen);
    return sum;
  };
  for (const node of profile.nodes) {
    const name = label(node);
    selfMs.set(name, (selfMs.get(name) ?? 0) + (node.hitCount ?? 0) * msPerHit);
    totalMs.set(name, Math.max(totalMs.get(name) ?? 0, subtreeHits(node.id) * msPerHit));
  }
  const rank = (map: Map<string, number>) =>
    [...map.entries()].sort((a, b) => b[1] - a[1]).slice(0, 30)
      .map(([name, ms]) => ({ name, ms: Math.round(ms) }));
  const out = { totalProfileMs: Math.round(hits * msPerHit), self: rank(selfMs), total: rank(totalMs) };
  process.stdout.write(`PROFILE ${JSON.stringify(out, null, 1)}\n`);
  if (process.env.PM_PROFILE_OUT) writeFileSync(process.env.PM_PROFILE_OUT, JSON.stringify(out, null, 1));
  await cdp.detach();
});
