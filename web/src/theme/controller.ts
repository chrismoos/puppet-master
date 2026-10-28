import { codeSurfaceVariables } from "@puppet-master/client-core/theme/codeSurface";
import { ANSI_COLOR_KEYS } from "@puppet-master/client-core/theme/terminalTheme";
import {
  BUILTIN_TERMINAL_THEME,
  type TerminalTheme,
} from "@puppet-master/client-core/theme/terminalTheme";
import type { GhosttyColor, GhosttyTheme } from "../ws/ghostty/core";

export interface TerminalThemeTarget {
  setTheme(theme: GhosttyTheme): void;
}

/** Applies one local user's persisted or temporary preview theme to every
 * terminal implementation without touching terminal/socket lifecycle. */
export class TerminalThemeController {
  private persisted: TerminalTheme = BUILTIN_TERMINAL_THEME;
  private previewTheme: TerminalTheme | null = null;
  private targets = new Set<TerminalThemeTarget>();

  register(target: TerminalThemeTarget): () => void {
    this.targets.add(target);
    target.setTheme(this.currentGhosttyTheme());
    return () => this.targets.delete(target);
  }

  setPersisted(theme: TerminalTheme | null): void {
    this.persisted = theme ?? BUILTIN_TERMINAL_THEME;
    if (!this.previewTheme) this.applyCurrent();
  }

  preview(theme: TerminalTheme): void {
    this.previewTheme = theme;
    this.applyCurrent();
  }

  cancelPreview(): void {
    if (!this.previewTheme) return;
    this.previewTheme = null;
    this.applyCurrent();
  }

  currentTheme(): TerminalTheme {
    return this.previewTheme ?? this.persisted;
  }

  currentGhosttyTheme(): GhosttyTheme {
    const theme = this.currentTheme();
    const palette = [...ANSI_COLOR_KEYS.map((key) => color(theme.colors[key]))];
    palette.push(...(theme.colors.extendedAnsi ?? []).map(color));
    return {
      foreground: color(theme.colors.foreground),
      background: color(theme.colors.background),
      cursor: color(theme.colors.cursor),
      palette,
      selectionBackground: theme.colors.selectionBackground,
    };
  }

  private applyCurrent(): void {
    for (const target of this.targets) target.setTheme(this.currentGhosttyTheme());
    publishThemeVariables(this.currentTheme());
  }
}

/// Publishes the palette as CSS custom properties so parts of the page
/// that are not terminals can be coloured from the same theme. The diff's
/// syntax highlighting reads these, which is what keeps one theme setting
/// covering the terminal and the code beside it, previews included.
///
/// The --code-* half is the palette resolved for the review diff, which
/// cannot use the raw ANSI slots: a palette designed around its own
/// background puts colours in them that vanish on it. The resolution is
/// done here rather than in CSS because it needs the measured contrast.
export function publishThemeVariables(theme: TerminalTheme): void {
  if (typeof document === "undefined") return;
  const style = document.documentElement.style;
  for (const [name, value] of Object.entries(theme.colors)) {
    // extendedAnsi is a list of the 256-colour slots, which nothing here
    // addresses by name.
    if (typeof value !== "string") continue;
    style.setProperty(`--term-${kebab(name)}`, value);
  }
  for (const [name, value] of Object.entries(codeSurfaceVariables(theme))) {
    style.setProperty(name, value);
  }
}

function kebab(name: string): string {
  return name.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
}

function color(value: string): GhosttyColor {
  return {
    r: Number.parseInt(value.slice(1, 3), 16),
    g: Number.parseInt(value.slice(3, 5), 16),
    b: Number.parseInt(value.slice(5, 7), 16),
  };
}
