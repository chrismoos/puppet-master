import type { ITheme } from "@xterm/xterm";
import { codeSurfaceVariables } from "@puppet-master/client-core/theme/codeSurface";
import {
  BUILTIN_TERMINAL_THEME,
  terminalThemeToXterm,
  type TerminalTheme,
} from "@puppet-master/client-core/theme/terminalTheme";

export interface TerminalThemeTarget {
  setTheme(theme: ITheme): void;
}

/** Applies one local user's persisted or temporary preview theme to every
 * terminal implementation without touching terminal/socket lifecycle. */
export class TerminalThemeController {
  private persisted: TerminalTheme = BUILTIN_TERMINAL_THEME;
  private previewTheme: TerminalTheme | null = null;
  private targets = new Set<TerminalThemeTarget>();

  register(target: TerminalThemeTarget): () => void {
    this.targets.add(target);
    target.setTheme(this.currentXtermTheme());
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

  currentXtermTheme(): ITheme {
    return terminalThemeToXterm(this.currentTheme());
  }

  private applyCurrent(): void {
    for (const target of this.targets) target.setTheme(this.currentXtermTheme());
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

export function freshXtermTheme(theme: ITheme): ITheme {
  return {
    ...theme,
    ...(theme.extendedAnsi ? { extendedAnsi: [...theme.extendedAnsi] } : {}),
  };
}
