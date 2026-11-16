import type { TerminalHistoryCursor } from "@puppet-master/client-core/ws/pty";
import compatibility from "../../../crates/pm-protocol/ghostty-snapshot-compatibility?raw";
import { authedFetch } from "../api/token";

const WARM_BYTES = 64 * 1024 * 1024;
const PRELOAD_CONCURRENCY = 2;
const RECENT_HISTORY_ROWS = 120;
const RECENT_PREVIEW_BYTES = 64 * 1024;
const RECENT_STORAGE = "pm.terminalRecentViews";
const MAX_RECENT_VIEWS = 1000;
const PRELOAD_RETRY_MS = 30_000;
const HTTP_GONE = 410;

export interface WarmTerminal { id: string; generation: string; sessionId: string; activity?: number }
export interface WarmSnapshot { data: Uint8Array; history: TerminalHistoryCursor; capturedAt: number }

export class TerminalHistoryError extends Error {
  constructor(readonly status: number) { super(`Terminal history unavailable (${status})`); }
}

export function decodeHistoryResponse(value: { data: string; history: TerminalHistoryCursor }): WarmSnapshot {
  const decoded = atob(value.data);
  const data = Uint8Array.from(decoded, (character) => character.charCodeAt(0));
  return { data, history: value.history, capturedAt: Date.now() };
}

export async function fetchTerminalHistory(terminal: WarmTerminal, history?: TerminalHistoryCursor, rows?: number, signal?: AbortSignal): Promise<WarmSnapshot> {
  const query = new URLSearchParams({ generation: terminal.generation, ghostty: compatibility.trim() });
  if (history) { query.set("snapshot", history.snapshotId); query.set("cursor", String(history.cursor)); }
  if (rows !== undefined) query.set("rows", String(rows));
  const response = await authedFetch(`/api/terminals/${terminal.id}/history?${query}`, { signal });
  if (!response.ok) throw new TerminalHistoryError(response.status);
  return decodeHistoryResponse(await response.json());
}

interface Entry extends WarmSnapshot { terminal: WarmTerminal; used: number; enriched: boolean }

export class TerminalWarmCache {
  private terminals = new Map<string, WarmTerminal>();
  private entries = new Map<string, Entry>();
  private running = new Set<string>();
  private failed = new Map<string, number>();
  private retryTimer: ReturnType<typeof setTimeout> | null = null;
  private recent: string[] = [];
  private selected: string | null = null;
  private bytes = 0;
  private revision = 0;

  constructor(private fetchSnapshot = fetchTerminalHistory, private budget = WARM_BYTES) {
    try {
      const stored: unknown = JSON.parse(localStorage.getItem(RECENT_STORAGE) ?? "[]");
      if (Array.isArray(stored)) this.recent = stored.filter((id): id is string => typeof id === "string").slice(0, MAX_RECENT_VIEWS);
    } catch { this.recent = []; }
  }

  reset(): void {
    this.terminals.clear();
    this.entries.clear();
    this.failed.clear();
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    this.selected = null;
    this.bytes = 0;
  }

  select(sessionId: string): void {
    if (this.selected === sessionId) return;
    this.selected = sessionId;
    for (const terminal of this.terminals.values()) {
      if (terminal.sessionId === sessionId) this.failed.delete(`${terminal.id}:${terminal.generation}`);
    }
    this.recent = [sessionId, ...this.recent.filter((id) => id !== sessionId)].slice(0, MAX_RECENT_VIEWS);
    try { localStorage.setItem(RECENT_STORAGE, JSON.stringify(this.recent)); } catch {}
    this.pump();
  }

  update(terminals: WarmTerminal[]): void {
    this.terminals = new Map(terminals.map((terminal) => [terminal.id, terminal]));
    const active = new Set(terminals.map((terminal) => `${terminal.id}:${terminal.generation}`));
    for (const key of this.failed.keys()) if (!active.has(key)) this.failed.delete(key);
    for (const [id, entry] of this.entries) {
      if (this.terminals.get(id)?.generation !== entry.terminal.generation) this.remove(id);
    }
    this.pump();
  }

  get(terminal: WarmTerminal): WarmSnapshot | null {
    const entry = this.entries.get(terminal.id);
    if (!entry || entry.terminal.generation !== terminal.generation) return null;
    entry.used = ++this.revision;
    return entry;
  }

  remember(terminal: WarmTerminal, snapshot: WarmSnapshot): void {
    const previous = this.entries.get(terminal.id);
    const total = this.bytes - (previous?.data.byteLength ?? 0) + snapshot.data.byteLength;
    if (total > this.budget) return;
    this.remove(terminal.id);
    this.failed.delete(`${terminal.id}:${terminal.generation}`);
    this.entries.set(terminal.id, { ...snapshot, terminal, used: ++this.revision, enriched: true });
    this.bytes = total;
  }

  stats(): { warmed: number; active: number; bytes: number; pending: number; order: string[] } {
    return { warmed: this.entries.size, active: this.terminals.size, bytes: this.bytes, pending: this.running.size, order: this.ordered().map((terminal) => terminal.sessionId) };
  }

  private ordered(): WarmTerminal[] {
    const ranks = new Map(this.recent.map((id, index) => [id, index]));
    return [...this.terminals.values()].sort((a, b) => {
      if (a.sessionId === this.selected) return -1;
      if (b.sessionId === this.selected) return 1;
      return (ranks.get(a.sessionId) ?? MAX_RECENT_VIEWS) - (ranks.get(b.sessionId) ?? MAX_RECENT_VIEWS)
        || (b.activity ?? 0) - (a.activity ?? 0);
    });
  }

  private remove(id: string): void {
    const entry = this.entries.get(id);
    if (entry) this.bytes -= entry.data.byteLength;
    this.entries.delete(id);
  }

  private pump(): void {
    if (this.retryTimer !== null) clearTimeout(this.retryTimer);
    this.retryTimer = null;
    while (this.running.size < PRELOAD_CONCURRENCY) {
      const ordered = this.ordered().filter((terminal) => !this.running.has(terminal.id) && (this.failed.get(`${terminal.id}:${terminal.generation}`) ?? 0) <= Date.now());
      const terminal = ordered.find((item) => !this.entries.has(item.id))
        ?? ordered.find((item) => !this.entries.get(item.id)?.enriched);
      if (!terminal) {
        const nextRetry = Math.min(...this.failed.values());
        if (Number.isFinite(nextRetry)) this.retryTimer = setTimeout(() => this.pump(), Math.max(0, nextRetry - Date.now()));
        return;
      }
      const existing = this.entries.get(terminal.id);
      this.running.add(terminal.id);
      void this.fetchSnapshot(terminal, existing?.history, existing ? RECENT_HISTORY_ROWS : undefined).then((snapshot) => {
        if (this.terminals.get(terminal.id)?.generation !== terminal.generation || this.entries.get(terminal.id) !== existing) return;
        const data = existing ? new Uint8Array(existing.data.length + snapshot.data.length) : snapshot.data;
        if (existing) { data.set(existing.data); data.set(snapshot.data, existing.data.length); }
        if (data.byteLength > this.budget) { this.failed.set(`${terminal.id}:${terminal.generation}`, Infinity); return; }
        this.remove(terminal.id);
        while (this.bytes + data.byteLength > this.budget && this.entries.size > 0) {
          const oldest = [...this.entries].sort((a, b) => a[1].used - b[1].used)[0]!;
          this.remove(oldest[0]);
          // Budget eviction must not immediately requeue the same session.
          this.failed.set(`${oldest[1].terminal.id}:${oldest[1].terminal.generation}`, Infinity);
        }
        const history = { ...snapshot.history, steps: (existing?.history.steps ?? 0) + snapshot.history.steps };
        this.entries.set(terminal.id, { ...snapshot, data, history, terminal, used: ++this.revision, enriched: Boolean(existing) || !snapshot.history.hasMore || data.byteLength >= RECENT_PREVIEW_BYTES });
        this.bytes += data.byteLength;
        this.failed.delete(`${terminal.id}:${terminal.generation}`);
      }).catch((error: unknown) => {
        if (this.terminals.get(terminal.id)?.generation !== terminal.generation || this.entries.get(terminal.id) !== existing) return;
        if (error instanceof TerminalHistoryError && error.status === HTTP_GONE) this.remove(terminal.id);
        this.failed.set(`${terminal.id}:${terminal.generation}`, Date.now() + PRELOAD_RETRY_MS);
      }).finally(() => {
        this.running.delete(terminal.id);
        this.pump();
      });
    }
  }
}
