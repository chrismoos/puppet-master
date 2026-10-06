// Narrow browser probe for the client render path. Synthesizes a fixed
// flood as real ServerMessage/PtyOutput frames and measures, per framing,
// the two costs coalescing would change: protobuf-es decode + xterm
// term.write drain (main-thread ms), plus requestAnimationFrame gaps
// (dropped frames) while it drains. Compares the current fragmentation
// against coalesced framings of the same bytes.
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { Terminal } from "@xterm/xterm";
import "@xterm/xterm/css/xterm.css";
import { ServerMessageSchema } from "../src/gen/pm/v1/pm_pb";

const TOTAL = 4 * 1024 * 1024;
const FRAME_BUDGET_MS = 1000 / 60;

function buildFrames(chunkSize: number): Uint8Array[] {
  const payload = new Uint8Array(chunkSize);
  for (let i = 0; i < chunkSize; i++) payload[i] = 97 + (i % 26); // printable a-z
  const frames: Uint8Array[] = [];
  let sent = 0;
  while (sent < TOTAL) {
    const size = Math.min(chunkSize, TOTAL - sent);
    const data = size === chunkSize ? payload : payload.subarray(0, size);
    const msg = create(ServerMessageSchema, {
      msg: {
        case: "ptyOutput",
        value: { sessionId: 1n, terminalId: 0n, generation: 1n, data, replay: false },
      },
    });
    frames.push(toBinary(ServerMessageSchema, msg));
    sent += size;
  }
  return frames;
}

interface Result {
  name: string;
  frames: number;
  avgFrameBytes: number;
  decodeMs: number;
  drainMs: number;
  rafFrames: number;
  droppedFrames: number;
  maxGapMs: number;
}

async function runScenario(name: string, chunkSize: number): Promise<Result> {
  const frames = buildFrames(chunkSize);
  const term = new Terminal({ cols: 120, rows: 40, scrollback: 5000 });
  const host = document.createElement("div");
  host.style.cssText = "position:absolute;left:-9999px;width:960px;height:640px";
  document.body.appendChild(host);
  term.open(host);

  const rafTimes: number[] = [];
  let sampling = true;
  const sample = (t: number) => {
    rafTimes.push(t);
    if (sampling) requestAnimationFrame(sample);
  };
  requestAnimationFrame(sample);
  await new Promise((r) => setTimeout(r, 150));
  rafTimes.length = 0;

  let decodeMs = 0;
  const t0 = performance.now();
  for (let i = 0; i < frames.length; i++) {
    const d0 = performance.now();
    const msg = fromBinary(ServerMessageSchema, frames[i]);
    const data = msg.msg.case === "ptyOutput" ? msg.msg.value.data : new Uint8Array();
    decodeMs += performance.now() - d0;
    if (i === frames.length - 1) {
      await new Promise<void>((res) => term.write(data, () => res()));
    } else {
      term.write(data);
    }
  }
  const drainMs = performance.now() - t0;
  sampling = false;

  const gaps: number[] = [];
  for (let i = 1; i < rafTimes.length; i++) gaps.push(rafTimes[i] - rafTimes[i - 1]);
  const droppedFrames = gaps.filter((g) => g > FRAME_BUDGET_MS * 1.5).length;
  const maxGapMs = gaps.length ? Math.max(...gaps) : 0;

  term.dispose();
  host.remove();
  const r1 = (n: number) => Math.round(n * 10) / 10;
  return {
    name,
    frames: frames.length,
    avgFrameBytes: Math.round(TOTAL / frames.length),
    decodeMs: r1(decodeMs),
    drainMs: r1(drainMs),
    rafFrames: rafTimes.length,
    droppedFrames,
    maxGapMs: r1(maxGapMs),
  };
}

async function main() {
  const results: Result[] = [];
  // ~3869 B/frame reproduces the measured server fragmentation (1084
  // frames for 4 MB); the others simulate coalescing to fewer, bigger
  // frames of the same total bytes.
  results.push(await runScenario("fragmented (~3.9KB)", 3869));
  results.push(await runScenario("coalesced 64KB", 64 * 1024));
  results.push(await runScenario("coalesced 256KB", 256 * 1024));
  (window as unknown as { __latencyResult: Result[] }).__latencyResult = results;
  const pre = document.createElement("pre");
  pre.id = "result";
  pre.textContent = JSON.stringify(results, null, 2);
  document.body.appendChild(pre);
}

void main();
