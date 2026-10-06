import { useEffect, useMemo, useRef, useState } from "react";
import {
  PUSH_WEB_IDLE_MINUTES_DEFAULT,
  PUSH_WEB_IDLE_MINUTES_KEY,
  PUSH_WEB_IDLE_MINUTES_MAX,
  applyAppearance,
  applyPushWebIdleMinutes,
  applyTerminalTheme,
  resetAppearance,
  resetTerminalTheme,
  applyUiTheme,
  resetUiTheme,
} from "../api/userSettings";
import { changePassword } from "../api/auth";
import { useAppState } from "../state/hooks";
import { useTerminalThemeController } from "../theme/context";
import {
  IMPORTED_CHOICE_KEY,
  TerminalThemePicker,
  activeChoiceKey,
  themeChoices,
  type ThemeChoice,
} from "./TerminalThemePicker";
import { UiThemePicker } from "./UiThemePicker";
import {
  parseAppearance,
  USER_APPEARANCE_KEY,
  type Appearance,
} from "@puppet-master/client-core/theme/appearance";
import {
  parseUiTheme,
  uiThemeLabel,
  USER_UI_THEME_KEY,
  type UiTheme,
} from "@puppet-master/client-core/theme/uiTheme";
import {
  ANSI_COLOR_KEYS,
  BUILTIN_TERMINAL_THEME,
  TERMINAL_THEME_MAX_BYTES,
  USER_TERMINAL_THEME_KEY,
  exportNativeTerminalTheme,
  parseGhosttyTerminalTheme,
  parseNativeTerminalTheme,
  validateTerminalTheme,
  type TerminalTheme,
  type ThemeImportResult,
} from "@puppet-master/client-core/theme/terminalTheme";
import { SettingsPageHead, errorMessage } from "./settingsParts";

/// Following the system is stored as no setting at all, so it is the
/// null choice here too rather than a third stored value.
const APPEARANCE_MODES: ReadonlyArray<{ value: Appearance | null; label: string }> = [
  { value: null, label: "System" },
  { value: "light", label: "Light" },
  { value: "dark", label: "Dark" },
];

export function AppearanceSettings() {
  const state = useAppState();
  const appearance = parseAppearance(state.userSettings.get(USER_APPEARANCE_KEY));
  const currentUiTheme = parseUiTheme(state.userSettings.get(USER_UI_THEME_KEY));
  const [modeBusy, setModeBusy] = useState(false);
  const [modeError, setModeError] = useState<string | null>(null);
  const [uiThemeBusy, setUiThemeBusy] = useState(false);
  const [uiThemeError, setUiThemeError] = useState<string | null>(null);
  const [uiThemeMessage, setUiThemeMessage] = useState<string | null>(null);

  const selectMode = async (mode: Appearance | null) => {
    if (modeBusy || mode === appearance) return;
    setModeBusy(true);
    setModeError(null);
    try {
      if (mode === null) await resetAppearance();
      else await applyAppearance(mode);
    } catch (reason) {
      setModeError(errorMessage(reason));
    } finally {
      setModeBusy(false);
    }
  };

  const selectUiTheme = async (theme: UiTheme) => {
    if (uiThemeBusy || theme === currentUiTheme) return;
    setUiThemeBusy(true);
    setUiThemeError(null);
    setUiThemeMessage(null);
    try {
      if (theme === "standard") {
        await resetUiTheme();
        setUiThemeMessage(`${uiThemeLabel(theme)} theme restored and synced.`);
      } else {
        const saved = await applyUiTheme(theme);
        setUiThemeMessage(`${uiThemeLabel(saved)} theme applied and synced to your account.`);
      }
    } catch (reason) {
      setUiThemeError(errorMessage(reason));
    } finally {
      setUiThemeBusy(false);
    }
  };

  return (
    <section className="set-page" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Appearance"
        description="Saved to your account and applied in every browser you sign in from."
      />
      <div className="ui-sect">
        <div className="ui-list">
          <div className="ui-row">
            <div className="main">
              <div className="title" id="appearance-mode-title">Mode</div>
              <div className="desc">System follows your operating system's light or dark setting.</div>
            </div>
            <div className="ctl">
              <div className="ui-seg" role="group" aria-labelledby="appearance-mode-title">
                {APPEARANCE_MODES.map((mode) => (
                  <button
                    key={mode.label}
                    type="button"
                    aria-pressed={appearance === mode.value}
                    disabled={modeBusy}
                    onClick={() => void selectMode(mode.value)}
                  >
                    {mode.label}
                  </button>
                ))}
              </div>
            </div>
          </div>
        </div>
        {modeError && <div className="form-error" role="alert">{modeError}</div>}
      </div>
      <div className="ui-sect">
        <div className="ui-sect-head">
          <h3>Interface theme</h3>
          <span>Colors and typeface of everything outside the terminal</span>
        </div>
        <UiThemePicker
          selected={currentUiTheme}
          disabled={uiThemeBusy}
          onSelect={(theme) => void selectUiTheme(theme)}
        />
        {uiThemeError && <div className="form-error" role="alert">{uiThemeError}</div>}
        {uiThemeMessage && <div className="set-status" role="status">{uiThemeMessage}</div>}
      </div>
    </section>
  );
}

export function TerminalThemeSettings() {
  const state = useAppState();
  const controller = useTerminalThemeController();
  const fileRef = useRef<HTMLInputElement>(null);
  const [imported, setImported] = useState<TerminalTheme | null>(null);
  const [selection, setSelection] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<string[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const stored = state.userSettings.get(USER_TERMINAL_THEME_KEY);
  const active = useMemo(() => {
    if (!stored) return BUILTIN_TERMINAL_THEME;
    try {
      return validateTerminalTheme(JSON.parse(stored));
    } catch (reason) {
      console.error("invalid user terminal theme in synchronized state", reason);
      return BUILTIN_TERMINAL_THEME;
    }
  }, [stored]);
  const choices = useMemo(() => themeChoices(active, imported), [active, imported]);
  const activeKey = useMemo(() => activeChoiceKey(choices, active), [choices, active]);
  const selectedKey = choices.some((choice) => choice.key === selection) ? selection! : activeKey;
  const shown = choices.find((choice) => choice.key === selectedKey)!.theme;
  const activeName = choices.find((choice) => choice.key === activeKey)!.theme.name;
  const previewing = selectedKey !== activeKey;

  // One effect keeps the controller on whatever the picker shows, so
  // choosing a bundled palette and importing a file preview identically.
  useEffect(() => {
    if (previewing) controller.preview(shown);
    else controller.cancelPreview();
  }, [controller, previewing, shown]);

  useEffect(() => () => controller.cancelPreview(), [controller]);

  const importFile = async (file: File | undefined) => {
    if (!file) return;
    setError(null);
    setMessage(null);
    try {
      const input = await readThemeFile(file);
      const result = parseImportedTheme(input, file.name);
      setImported(result.theme);
      setSelection(IMPORTED_CHOICE_KEY);
      setWarnings(result.warnings);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      if (fileRef.current) fileRef.current.value = "";
    }
  };

  const cancelPreview = () => {
    setSelection(null);
    setWarnings([]);
    setError(null);
    setMessage("Preview canceled; active theme restored.");
  };

  const select = (choice: ThemeChoice) => {
    setSelection(choice.key);
    setError(null);
    setMessage(null);
    if (choice.key !== IMPORTED_CHOICE_KEY) setWarnings([]);
  };

  const apply = async () => {
    if (!previewing || busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await applyTerminalTheme(shown);
      controller.setPersisted(saved);
      setSelection(null);
      setWarnings([]);
      setMessage(`${saved.name} applied and synced to your account.`);
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const reset = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await resetTerminalTheme();
      controller.setPersisted(null);
      setImported(null);
      setSelection(null);
      setWarnings([]);
      setMessage("Built-in Puppet Master terminal theme restored and synced.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="set-page" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Terminal theme"
        description="Colors for embedded terminals only. Pick one to preview it, then apply."
        actions={<>
          <input
            ref={fileRef}
            className="visually-hidden"
            type="file"
            aria-label="Choose a Puppet Master JSON or Ghostty theme file"
            onChange={(event) => void importFile(event.target.files?.[0])}
          />
          <button
            type="button"
            className="btn btn-quiet"
            onClick={() => void reset()}
            disabled={busy || (!stored && !previewing && !imported)}
          >
            Reset
          </button>
          <button type="button" className="btn" onClick={() => fileRef.current?.click()} disabled={busy}>
            Import
          </button>
          <button type="button" className="btn" onClick={() => exportTheme(shown)} disabled={busy}>
            Export JSON
          </button>
        </>}
      />
      <div className="set-term-themes">
        <TerminalThemePicker
          choices={choices}
          selectedKey={selectedKey}
          activeKey={activeKey}
          disabled={busy}
          onSelect={select}
        />
        <div className="set-term-side">
          <TerminalThemePreview theme={shown} />
          <div className={`set-preview-bar${previewing ? " previewing" : ""}`}>
            <span className="msg" role="status">
              {previewing
                ? `Previewing ${shown.name}. Your terminals still use ${activeName}.`
                : `${shown.name} is active.`}
            </span>
            {previewing && (
              <>
                <button type="button" className="btn btn-quiet" onClick={cancelPreview} disabled={busy}>
                  Cancel
                </button>
                <button type="button" className="btn btn-primary" onClick={() => void apply()} disabled={busy}>
                  {busy ? "Saving…" : "Apply theme"}
                </button>
              </>
            )}
          </div>
          {error && <div className="form-error" role="alert">{error}</div>}
          {warnings.length > 0 && (
            <div className="set-diagnostics" role="status" aria-label="theme import warnings">
              <strong>Import diagnostics</strong>
              <ul>{warnings.map((warning) => <li key={warning}>{warning}</li>)}</ul>
            </div>
          )}
          {message && <div className="set-status" role="status">{message}</div>}
          <p className="ui-hint">
            Import a Puppet Master JSON or Ghostty color theme, up to 64 KiB. Includes, paths,
            commands, and remote content in a file are never evaluated.
          </p>
        </div>
      </div>
    </section>
  );
}

/** The daemon holds the same floor and rejects a short password itself;
 * this only spares the reader a round trip. */
export const MIN_PASSWORD_CHARS = 8;

export function passwordProblem(
  current: string,
  next: string,
  confirm: string,
): string | null {
  if (current === "" || next === "" || confirm === "") return "Fill in every field.";
  if (next !== confirm) return "The new passwords do not match.";
  if ([...next].length < MIN_PASSWORD_CHARS) {
    return `The new password must be at least ${MIN_PASSWORD_CHARS} characters.`;
  }
  if (next === current) return "The new password must differ from the current one.";
  return null;
}

export function PasswordSettings() {
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [confirm, setConfirm] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const submit = async (event: React.FormEvent) => {
    event.preventDefault();
    if (busy) return;
    const problem = passwordProblem(current, next, confirm);
    if (problem) {
      setMessage(null);
      setError(problem);
      return;
    }
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await changePassword(current, next);
      setCurrent("");
      setNext("");
      setConfirm("");
      setMessage("Password changed. Your other browsers and devices were signed out.");
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="set-page" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Password"
        description="Changing it signs out your other browsers. This one stays signed in, and devices stay enrolled because they carry their own token."
      />
      <form className="ui-form-card set-narrow-card" onSubmit={(e) => void submit(e)}>
        <div className="body">
          <div className="ui-field">
            <label htmlFor="password-current">Current password</label>
            <input
              id="password-current"
              className="ui-input ui-w-md"
              type="password"
              autoComplete="current-password"
              value={current}
              disabled={busy}
              onChange={(e) => setCurrent(e.target.value)}
            />
          </div>
          <div className="ui-field">
            <label htmlFor="password-new">New password</label>
            <input
              id="password-new"
              className="ui-input ui-w-md"
              type="password"
              autoComplete="new-password"
              aria-describedby="password-new-hint"
              value={next}
              disabled={busy}
              onChange={(e) => setNext(e.target.value)}
            />
            <span className="ui-hint" id="password-new-hint">At least {MIN_PASSWORD_CHARS} characters.</span>
          </div>
          <div className="ui-field">
            <label htmlFor="password-confirm">Confirm new password</label>
            <input
              id="password-confirm"
              className="ui-input ui-w-md"
              type="password"
              autoComplete="new-password"
              value={confirm}
              disabled={busy}
              onChange={(e) => setConfirm(e.target.value)}
            />
          </div>
          {error && <div className="form-error" role="alert">{error}</div>}
          {message && <div className="set-status" role="status">{message}</div>}
        </div>
        <footer>
          <button type="submit" className="btn btn-primary btn-lg" disabled={busy}>
            {busy ? "Changing…" : "Change password"}
          </button>
        </footer>
      </form>
    </section>
  );
}

/** The idle threshold that decides when push resumes after web use. */
export function NotificationSettings() {
  const state = useAppState();
  const stored = state.userSettings.get(PUSH_WEB_IDLE_MINUTES_KEY);
  const active = (stored === undefined ? null : parseIdleMinutes(stored)) ?? PUSH_WEB_IDLE_MINUTES_DEFAULT;
  const [draft, setDraft] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);
  const shown = draft ?? String(active);
  const parsed = parseIdleMinutes(shown);

  const save = async (minutes: number | null) => {
    if (minutes === null || busy) return;
    if (minutes === active) {
      setDraft(null);
      return;
    }
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const saved = await applyPushWebIdleMinutes(minutes);
      setDraft(null);
      setMessage(idleThresholdMessage(saved));
    } catch (reason) {
      setError(errorMessage(reason));
    } finally {
      setBusy(false);
    }
  };

  const step = (direction: -1 | 1) => {
    const next = Math.min(PUSH_WEB_IDLE_MINUTES_MAX, Math.max(0, (parsed ?? active) + direction));
    setDraft(String(next));
    void save(next);
  };

  return (
    <section className="set-page" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Notifications"
        description="How your phone and this browser share the job of getting your attention."
      />
      <div className="ui-list">
        <div className="ui-row">
          <div className="main">
            <div className="title" id="push-quiet-title">Hold phone notifications while I work here</div>
            <div className="desc">
              Your phone stays quiet while you click, type, scroll, or touch in this browser, and
              resumes after you have been idle this long. A tab left open does not count. Set 0 to
              always notify your phone.
            </div>
          </div>
          <div className="ctl">
            <span className="ui-stepper">
              <span className="box">
                <button type="button" aria-label="Decrease" disabled={busy} onClick={() => step(-1)}>−</button>
                <input
                  id="push-web-idle-minutes"
                  type="text"
                  inputMode="numeric"
                  aria-label="Idle minutes"
                  aria-describedby="push-quiet-title"
                  value={shown}
                  disabled={busy}
                  onChange={(event) => {
                    setDraft(event.target.value);
                    setError(null);
                    setMessage(null);
                  }}
                  onBlur={() => void save(parsed)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") event.currentTarget.blur();
                    else if (event.key === "Escape") setDraft(null);
                  }}
                />
                <button type="button" aria-label="Increase" disabled={busy} onClick={() => step(1)}>+</button>
              </span>
              minutes
            </span>
          </div>
        </div>
      </div>
      {parsed === null && (
        <div className="form-error" role="alert">
          Enter a whole number of minutes between 0 and {PUSH_WEB_IDLE_MINUTES_MAX}.
        </div>
      )}
      {error && <div className="form-error" role="alert">{error}</div>}
      <p className="ui-hint set-below" role="status">
        {busy ? "Saving…" : message ?? "Changes save as you make them."}
      </p>
    </section>
  );
}

/** The threshold as a whole number of minutes, or null if unusable. */
export function parseIdleMinutes(input: string): number | null {
  const trimmed = input.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const minutes = Number(trimmed);
  return minutes <= PUSH_WEB_IDLE_MINUTES_MAX ? minutes : null;
}

export function idleThresholdMessage(minutes: number): string {
  if (minutes === 0) return "Your phone is notified even while you are working here.";
  const label = minutes === 1 ? "a minute" : `${minutes} minutes`;
  return `Notifications wait until you have been idle here for ${label}.`;
}

export function TerminalThemePreview({ theme }: { theme: TerminalTheme }) {
  return (
    <div
      className="set-term-preview"
      style={{ borderColor: theme.colors.brightBlack }}
      aria-label={`${theme.name} terminal color preview`}
    >
      <div className="screen" style={{ background: theme.colors.background, color: theme.colors.foreground }}>
        <div className="set-term-preview-path">~/puppet-master</div>
        <div><span style={{ color: theme.colors.green }}>✓</span> tests passing</div>
        <div><span style={{ color: theme.colors.blue }}>git</span> status <span style={{ color: theme.colors.yellow }}>--short</span></div>
        <div><span style={{ color: theme.colors.red }}> M</span> web/src/views/SettingsPage.tsx</div>
        <div>
          <span style={{ background: theme.colors.selectionBackground, color: theme.colors.selectionForeground }}>
            selected terminal text
          </span>
        </div>
        <div>
          <span style={{ color: theme.colors.magenta }}>❯</span>{" "}
          <span className="set-term-preview-cursor" style={{ background: theme.colors.cursor, color: theme.colors.cursorAccent }}> </span>
        </div>
      </div>
      <div className="ansi" aria-label="ANSI 0 through 15">
        {ANSI_COLOR_KEYS.map((key, index) => (
          <span key={key} title={`${index}: ${key} ${theme.colors[key]}`} style={{ background: theme.colors[key] }} />
        ))}
      </div>
    </div>
  );
}

export function parseImportedTheme(input: string, filename: string): ThemeImportResult {
  const importedName = filename.replace(/\.[^.]+$/, "").trim() || "Imported Ghostty";
  try {
    return parseNativeTerminalTheme(input);
  } catch (nativeError) {
    // A malformed native object must keep its strict JSON diagnostic. Only
    // fall back when the contents contain a color setting understood by the
    // Ghostty parser, independent of the file's name or MIME type.
    if (input.trimStart().startsWith("{") || !hasGhosttyColorSetting(input)) {
      throw nativeError;
    }
  }
  return parseGhosttyTerminalTheme(input, importedName);
}

function hasGhosttyColorSetting(input: string): boolean {
  return input.split(/\r?\n/).some((rawLine) => {
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) return false;
    const equals = line.indexOf("=");
    if (equals < 0) return false;
    const key = line.slice(0, equals).trim().toLowerCase();
    return key === "palette" || [
      "foreground", "background", "cursor-color", "cursor-text",
      "selection-foreground", "selection-background",
    ].includes(key);
  });
}

export async function readThemeFile(file: Blob): Promise<string> {
  if (file.size > TERMINAL_THEME_MAX_BYTES) {
    throw new Error("Theme exceeds the 64 KiB import limit");
  }
  const bytes = await file.arrayBuffer();
  if (bytes.byteLength > TERMINAL_THEME_MAX_BYTES) {
    throw new Error("Theme exceeds the 64 KiB import limit");
  }
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
  } catch {
    throw new Error("Theme file must be valid UTF-8");
  }
}

function exportTheme(theme: TerminalTheme): void {
  const blob = new Blob([exportNativeTerminalTheme(theme)], { type: "application/json" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = `${theme.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "terminal-theme"}.json`;
  link.click();
  URL.revokeObjectURL(url);
}
