import { create } from "@bufbuild/protobuf";
import type { IBufferLine, ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  ItemSchema,
  ProjectSchema,
  SessionSchema,
  TerminalSchema,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import { itemKey } from "@puppet-master/client-core/state/reducer";
import {
  HttpTerminalLinkProvider,
  openTerminalHttpLink,
  isSafeHttpUrl,
  isTerminalLinkActivation,
  isTerminalPmLinkActivation,
  PmTerminalLinkProvider,
  registerPmTerminalLinkProvider,
  resolveTerminalPmLink,
  terminalHttpLinksForLine,
  terminalPmLinksForLine,
} from "./terminalLinks";

function line(text: string, cols: number, isWrapped = false): IBufferLine {
  const chars = [...text.padEnd(cols, " ")];
  return {
    isWrapped,
    length: cols,
    getCell: (x: number) => ({
      getChars: () => chars[x] ?? "",
      getWidth: () => 1,
    }),
  } as unknown as IBufferLine;
}

function terminal(lines: IBufferLine[], cols: number): Terminal {
  return {
    cols,
    modes: { mouseTrackingMode: "none" },
    buffer: { active: { getLine: (index: number) => lines[index] } },
  } as unknown as Terminal;
}

function state(): AppState {
  const largeId = 9_007_199_254_740_993n;
  const projects = new Map([
    ["10", create(ProjectSchema, { id: 10n, bucketId: 1n })],
    ["20", create(ProjectSchema, { id: 20n, bucketId: 2n })],
  ]);
  const sessions = new Map([
    ["100", create(SessionSchema, { id: 100n, projectId: 10n })],
    [largeId.toString(), create(SessionSchema, { id: largeId, projectId: 10n })],
    ["200", create(SessionSchema, { id: 200n, projectId: 20n })],
  ]);
  return {
    ...initialState,
    hydrated: true,
    projects,
    sessions,
    terminals: new Map([
      ["1000", create(TerminalSchema, { id: 1000n, sessionId: 100n })],
    ]),
    items: new Map([
      [itemKey(1n, largeId), create(ItemSchema, { id: largeId, bucketId: 1n })],
      [itemKey(2n, 2n), create(ItemSchema, { id: 2n, bucketId: 2n })],
    ]),
  };
}

describe("terminalPmLinksForLine", () => {
  it("matches strict references with surrounding punctuation", () => {
    const term = terminal([line("(pm:item/12), pm:session/37.", 40)], 40);
    expect(terminalPmLinksForLine(term, 1)).toEqual([
      {
        text: "pm:item/12",
        range: { start: { x: 2, y: 1 }, end: { x: 11, y: 1 } },
      },
      {
        text: "pm:session/37",
        range: { start: { x: 15, y: 1 }, end: { x: 27, y: 1 } },
      },
    ]);
  });

  it("rejects identifier-adjacent and malformed paths", () => {
    const term = terminal([
      line("xpm:item/1 pm:item/2x pm:item/3/4/5 pm:project/5", 60),
    ], 60);
    expect(terminalPmLinksForLine(term, 1)).toEqual([]);
  });

  it("maps a reference across wrapped rows", () => {
    const term = terminal([
      line("prefix pm:it", 12),
      line("em/900719925", 12, true),
      line("4740993 tail", 12, true),
    ], 12);
    expect(terminalPmLinksForLine(term, 2)).toEqual([
      {
        text: "pm:item/9007199254740993",
        range: { start: { x: 8, y: 1 }, end: { x: 7, y: 3 } },
      },
    ]);
  });

  it("matches the displayed label of an OSC hyperlink", () => {
    const term = terminal([line("pm:session/37", 20)], 20);
    expect(terminalPmLinksForLine(term, 1).map((link) => link.text)).toEqual(["pm:session/37"]);
  });
});

describe("terminalHttpLinksForLine", () => {
  it("matches HTTP(S) URLs, trims prose punctuation, and preserves balanced URL brackets", () => {
    const term = terminal([
      line("See (https://example.test/a_(b)), then HTTP://localhost:8080/x?q=1#two.", 90),
    ], 90);
    expect(terminalHttpLinksForLine(term, 1)).toEqual([
      {
        text: "https://example.test/a_(b)",
        range: { start: { x: 6, y: 1 }, end: { x: 31, y: 1 } },
      },
      {
        text: "HTTP://localhost:8080/x?q=1#two",
        range: { start: { x: 40, y: 1 }, end: { x: 70, y: 1 } },
      },
    ]);
  });

  it("maps wrapped URLs and rejects malformed or identifier-adjacent candidates", () => {
    const wrapped = terminal([
      line("go https://exa", 14),
      line("mple.test/path", 14, true),
    ], 14);
    expect(terminalHttpLinksForLine(wrapped, 2)).toEqual([
      {
        text: "https://example.test/path",
        range: { start: { x: 4, y: 1 }, end: { x: 14, y: 2 } },
      },
    ]);

    const invalid = terminal([
      line("xhttps://example.test https:// javascript:alert(1) file:///tmp/a https://bad%zz", 90),
    ], 90);
    expect(terminalHttpLinksForLine(invalid, 1)).toEqual([]);
  });
});

describe("isSafeHttpUrl", () => {
  it("allows only well-formed HTTP(S) URLs with a host", () => {
    expect(isSafeHttpUrl("http://localhost:3000/path")).toBe(true);
    expect(isSafeHttpUrl("https://example.test/%E2%9C%93")).toBe(true);
    expect(isSafeHttpUrl("javascript:alert(1)")).toBe(false);
    expect(isSafeHttpUrl("file:///tmp/a")).toBe(false);
    expect(isSafeHttpUrl("https://")).toBe(false);
    expect(isSafeHttpUrl("https://example.test/%zz")).toBe(false);
  });
});

describe("resolveTerminalPmLink", () => {
  it("routes same-bucket items and sessions without Number coercion", () => {
    const current = state();
    const largeId = "9007199254740993";
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, `pm:item/1/${largeId}`))
      .toEqual({ path: `/bucket/1/item/${largeId}` });
    expect(resolveTerminalPmLink(current, { kind: "terminal", id: "1000" }, `pm:session/${largeId}`))
      .toEqual({ path: `/session/${largeId}` });
  });

  it("rejects stale, cross-bucket, malformed, and unhydrated targets", () => {
    const current = state();
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/2/2").error)
      .toContain("outside this terminal's bucket");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/2").error)
      .toContain("unqualified");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:session/999").error)
      .toContain("missing or inaccessible");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/nope").error)
      .toContain("Unsupported");
    expect(resolveTerminalPmLink(initialState, { kind: "session", id: "100" }, "pm:item/1/1").error)
      .toContain("still loading");
  });
});

describe("PmTerminalLinkProvider", () => {
  it("accepts plain primary-click, finishes pointer dispatch, and revalidates on activation", async () => {
    const term = terminal([line("pm:session/9007199254740993", 32)], 32);
    let current = state();
    const go = vi.fn();
    const reportError = vi.fn();
    const prepareNavigation = vi.fn();
    const provider = new PmTerminalLinkProvider(
      term,
      () => current,
      { kind: "session", id: "100" },
      go,
      reportError,
      undefined,
      prepareNavigation,
    );
    let links: ILink[] | undefined;
    provider.provideLinks(1, (provided) => { links = provided; });
    const normal = { button: 0, shiftKey: false, altKey: false, preventDefault: vi.fn(), stopPropagation: vi.fn() } as unknown as MouseEvent;
    links?.[0].activate(normal, links[0].text);
    expect(go).not.toHaveBeenCalled();
    await Promise.resolve();
    expect(go).toHaveBeenCalledWith("/session/9007199254740993");
    expect(normal.preventDefault).toHaveBeenCalledOnce();
    expect(normal.stopPropagation).not.toHaveBeenCalled();
    expect(prepareNavigation).toHaveBeenCalledOnce();

    const modified = { button: 0, shiftKey: true, altKey: false, preventDefault: vi.fn(), stopPropagation: vi.fn() } as unknown as MouseEvent;
    links?.[0].activate(modified, links[0].text);
    await Promise.resolve();
    expect(go).toHaveBeenCalledTimes(2);

    current = { ...current, sessions: new Map([["100", current.sessions.get("100")!]]) };
    links?.[0].activate(modified, links[0].text);
    expect(reportError).toHaveBeenCalledWith("Session 9007199254740993 is missing or inaccessible.");
  });

  it("registers PM and HTTP providers without changing terminal options", () => {
    const dispose = vi.fn();
    const registerLinkProvider = vi.fn((_provider: ILinkProvider) => ({ dispose }));
    const element = {
      addEventListener: vi.fn(),
      removeEventListener: vi.fn(),
      querySelector: vi.fn(() => null),
    } as unknown as HTMLElement;
    const term = { registerLinkProvider, element, modes: { mouseTrackingMode: "none" } } as unknown as Terminal;
    const registration = registerPmTerminalLinkProvider(
      term,
      { getState: () => state() },
      { kind: "session", id: "100" },
    );
    expect(registerLinkProvider).toHaveBeenCalledTimes(2);
    expect(registerLinkProvider.mock.calls[0][0]).toBeInstanceOf(PmTerminalLinkProvider);
    expect(registerLinkProvider.mock.calls[1][0]).toBeInstanceOf(HttpTerminalLinkProvider);
    expect((term as unknown as { options?: unknown }).options).toBeUndefined();
    registration.dispose();
    expect(dispose).toHaveBeenCalledTimes(2);
  });
});

describe("HttpTerminalLinkProvider", () => {
  it("opens safe URLs for plain and platform modifier clicks but not drags", () => {
    const term = terminal([line("https://example.test/path", 32)], 32);
    const open = vi.fn();
    let dragged = false;
    const provider = new HttpTerminalLinkProvider(term, open, () => dragged);
    let links: ILink[] | undefined;
    provider.provideLinks(1, (provided) => { links = provided; });
    const activate = (overrides: Partial<MouseEvent> = {}) => {
      const event = {
        button: 0,
        shiftKey: false,
        altKey: false,
        ctrlKey: false,
        metaKey: false,
        preventDefault: vi.fn(),
        stopPropagation: vi.fn(),
        ...overrides,
      } as unknown as MouseEvent;
      links?.[0].activate(event, links[0].text);
      return event;
    };

    for (const modifier of [{}, { metaKey: true }, { ctrlKey: true }, { shiftKey: true }]) {
      const event = activate(modifier);
      expect(event.preventDefault).toHaveBeenCalledOnce();
    }
    expect(open).toHaveBeenCalledTimes(4);
    expect(open).toHaveBeenLastCalledWith("https://example.test/path");

    dragged = true;
    const drag = activate();
    expect(open).toHaveBeenCalledTimes(4);
    expect(drag.preventDefault).not.toHaveBeenCalled();
  });

  it("requires Shift or Alt while PTY mouse tracking is active", () => {
    const term = terminal([line("https://example.test", 24)], 24);
    (term.modes as { mouseTrackingMode: Terminal["modes"]["mouseTrackingMode"] }).mouseTrackingMode = "any";
    const open = vi.fn();
    const provider = new HttpTerminalLinkProvider(term, open);
    let links: ILink[] | undefined;
    provider.provideLinks(1, (provided) => { links = provided; });
    const event = (shiftKey: boolean, altKey = false) => ({
      button: 0,
      shiftKey,
      altKey,
      ctrlKey: false,
      metaKey: false,
      preventDefault: vi.fn(),
      stopPropagation: vi.fn(),
    } as unknown as MouseEvent);
    links?.[0].activate(event(false), links[0].text);
    links?.[0].activate(event(true), links[0].text);
    links?.[0].activate(event(false, true), links[0].text);
    expect(open).toHaveBeenCalledTimes(2);
  });
});

describe("isTerminalPmLinkActivation", () => {
  it("accepts plain clicks normally, requires a modifier for PTY mouse tracking, and rejects drags", () => {
    const plain = { button: 0, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false };
    expect(isTerminalPmLinkActivation(plain, "none")).toBe(true);
    expect(isTerminalPmLinkActivation(plain, "vt200")).toBe(false);
    expect(isTerminalPmLinkActivation({ ...plain, shiftKey: true }, "vt200")).toBe(true);
    expect(isTerminalPmLinkActivation({ ...plain, altKey: true }, "any")).toBe(true);
    expect(isTerminalPmLinkActivation({ ...plain, shiftKey: true }, "drag", true)).toBe(false);
    expect(isTerminalPmLinkActivation({ ...plain, button: 1, shiftKey: true }, "none")).toBe(false);
  });

  it("treats Command, Ctrl, and Shift clicks intentionally in normal terminal mode", () => {
    const plain = { button: 0, shiftKey: false, altKey: false, ctrlKey: false, metaKey: false };
    expect(isTerminalLinkActivation({ ...plain, metaKey: true }, "none")).toBe(true);
    expect(isTerminalLinkActivation({ ...plain, ctrlKey: true }, "none")).toBe(true);
    expect(isTerminalLinkActivation({ ...plain, shiftKey: true }, "none")).toBe(true);
  });
});


describe("opening HTTP review links", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("opens a same-app review as a closable fullscreen review tab", () => {
    const location = new URL("http://localhost/forwards/12/");
    const open = vi.fn();
    vi.stubGlobal("location", location);
    vi.stubGlobal("window", { open });
    const href = `${location.href}#/review/4?file=a.txt&thread=2`;
    openTerminalHttpLink(href);
    expect(open).toHaveBeenCalledWith(href, "_blank");
  });

  it("keeps unrelated URLs isolated from the opener", () => {
    const location = new URL("http://localhost/forwards/12/");
    const open = vi.fn();
    vi.stubGlobal("location", location);
    vi.stubGlobal("window", { open });
    for (const href of [
      `${location.href}#/session/4`,
      `${location.origin}/forwards/13/#/review/4`,
      "http://127.0.0.1/forwards/12/#/review/4",
    ]) {
      openTerminalHttpLink(href);
      expect(open).toHaveBeenLastCalledWith(href, "_blank", "noopener,noreferrer");
    }
  });
});
