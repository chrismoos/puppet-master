import { describe, expect, it } from "vitest";
import type { ITheme } from "@xterm/xterm";
import {
  publishThemeVariables,
  TerminalThemeController,
  type TerminalThemeTarget,
} from "./controller";
import { BUILTIN_TERMINAL_THEME, type TerminalTheme } from "@puppet-master/client-core/theme/terminalTheme";

class LifecycleProbe implements TerminalThemeTarget {
  readonly xterm = { id: Symbol("xterm") };
  readonly pty = { id: Symbol("pty"), attach: 0, replay: 0, resize: 0, reconnect: 0 };
  readonly dom = { id: Symbol("dom") };
  readonly buffer = { contents: "preserved", selection: "selected", viewport: 42, focused: true };
  themes: ITheme[] = [];

  setTheme(theme: ITheme): void {
    this.themes.push(theme);
  }
}

function imported(name: string, background: string): TerminalTheme {
  return {
    ...BUILTIN_TERMINAL_THEME,
    name,
    colors: { ...BUILTIN_TERMINAL_THEME.colors, background },
  };
}

describe("live terminal theme controller", () => {
  it("updates every warm stage/workspace target and retains the theme for future terminals", () => {
    const controller = new TerminalThemeController();
    const warmStageLayers = [new LifecycleProbe(), new LifecycleProbe(), new LifecycleProbe()];
    const simultaneousWorkspacePanes = [new LifecycleProbe(), new LifecycleProbe()];
    const unregister = [...warmStageLayers, ...simultaneousWorkspacePanes]
      .map((probe) => controller.register(probe));
    const identities = [...warmStageLayers, ...simultaneousWorkspacePanes]
      .map((probe) => ({ xterm: probe.xterm, pty: probe.pty, dom: probe.dom, buffer: { ...probe.buffer } }));

    controller.setPersisted(imported("Synced", "#101010"));
    const probes = [...warmStageLayers, ...simultaneousWorkspacePanes];
    expect(probes.every((probe) => probe.themes.at(-1)?.background === "#101010")).toBe(true);
    expect(new Set(probes.map((probe) => probe.themes.at(-1))).size).toBe(probes.length);
    probes.forEach((probe, index) => {
      expect(probe.xterm).toBe(identities[index].xterm);
      expect(probe.pty).toBe(identities[index].pty);
      expect(probe.dom).toBe(identities[index].dom);
      expect(probe.buffer).toEqual(identities[index].buffer);
      expect(probe.pty).toMatchObject({ attach: 0, replay: 0, resize: 0, reconnect: 0 });
    });

    const future = new LifecycleProbe();
    controller.register(future);
    expect(future.themes).toHaveLength(1);
    expect(future.themes[0].background).toBe("#101010");
    unregister.forEach((dispose) => dispose());
  });

  it("keeps preview local and cancel restores the latest persisted theme", () => {
    const controller = new TerminalThemeController();
    const local = new LifecycleProbe();
    controller.register(local);
    controller.setPersisted(imported("Saved", "#111111"));
    controller.preview(imported("Preview", "#222222"));
    controller.setPersisted(imported("Same-user remote update", "#333333"));
    expect(local.themes.at(-1)?.background).toBe("#222222");
    controller.cancelPreview();
    expect(local.themes.at(-1)?.background).toBe("#333333");
  });
});

describe("publishThemeVariables", () => {
  it("does nothing where there is no document", () => {
    // The review page renders server-side in tests, and a theme applied
    // during that render must not reach for the DOM.
    expect(() => publishThemeVariables(BUILTIN_TERMINAL_THEME)).not.toThrow();
  });
});
