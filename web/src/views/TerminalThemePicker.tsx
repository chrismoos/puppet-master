import {
  BUILTIN_TERMINAL_THEMES,
  builtinTerminalThemeMatching,
  sameTerminalPalette,
} from "@puppet-master/client-core/theme/builtinTerminalThemes";
import {
  ANSI_COLOR_KEYS,
  terminalThemeAppearance,
  type TerminalTheme,
} from "@puppet-master/client-core/theme/terminalTheme";

/// The six chromatic ANSI colors, which tell palettes apart at chip size
/// better than black and white do.
const SWATCH_COLOR_KEYS = ANSI_COLOR_KEYS.slice(1, 7);

export interface ThemeChoice {
  key: string;
  theme: TerminalTheme;
  origin: "built-in" | "imported";
  appearance: "light" | "dark";
}

export const IMPORTED_CHOICE_KEY = "imported";

/// The bundled palettes and the user's imported one as a single list, so
/// choosing a built-in and choosing an import are the same gesture and a
/// user can go back to a built-in without importing anything again.
///
/// `imported` is this session's most recent import; a stored theme that
/// matches no bundled palette takes the imported slot instead, which is
/// what puts a previously applied import back in the list on reload.
export function themeChoices(active: TerminalTheme, imported: TerminalTheme | null): ThemeChoice[] {
  const choices: ThemeChoice[] = BUILTIN_TERMINAL_THEMES.map((theme) => ({
    key: `builtin:${theme.name}`,
    theme,
    origin: "built-in",
    appearance: terminalThemeAppearance(theme),
  }));
  const custom = imported ?? (builtinTerminalThemeMatching(active) ? null : active);
  if (custom) {
    choices.push({
      key: IMPORTED_CHOICE_KEY,
      theme: custom,
      origin: "imported",
      appearance: terminalThemeAppearance(custom),
    });
  }
  return choices;
}

/// The choice the persisted theme corresponds to. An import whose colours
/// happen to equal a bundled palette still wins, so the card the user
/// actually applied is the one marked active rather than its twin.
export function activeChoiceKey(choices: ThemeChoice[], active: TerminalTheme): string {
  const custom = choices.find((choice) => choice.key === IMPORTED_CHOICE_KEY);
  if (custom && custom.theme.name === active.name && sameTerminalPalette(custom.theme, active)) {
    return IMPORTED_CHOICE_KEY;
  }
  const builtin = builtinTerminalThemeMatching(active);
  if (builtin) return `builtin:${builtin.name}`;
  return custom ? IMPORTED_CHOICE_KEY : choices[0].key;
}

export function TerminalThemePicker({
  choices,
  selectedKey,
  activeKey,
  disabled,
  onSelect,
}: {
  choices: ThemeChoice[];
  selectedKey: string;
  activeKey: string;
  disabled: boolean;
  onSelect: (choice: ThemeChoice) => void;
}) {
  return (
    <ul className="ui-list set-theme-list" aria-label="Terminal theme">
      {choices.map((choice) => {
        const selected = choice.key === selectedKey;
        const origin = choice.origin === "built-in" ? "Built in" : "Imported";
        const appearance = choice.appearance === "light" ? "Light" : "Dark";
        return (
          <li key={choice.key}>
            <button
              type="button"
              className="set-theme-row"
              aria-pressed={selected}
              disabled={disabled}
              onClick={() => onSelect(choice)}
            >
              <span
                className="chip"
                style={{ background: choice.theme.colors.background }}
                aria-hidden="true"
              >
                {SWATCH_COLOR_KEYS.map((key) => (
                  <i key={key} style={{ background: choice.theme.colors[key] }} />
                ))}
              </span>
              <span className="name">
                <b>{choice.theme.name}</b>
                <small>{origin} · {appearance}</small>
              </span>
              {choice.key === activeKey
                ? <span className="ui-badge ok">Active</span>
                : selected ? <span className="ui-badge warn">Preview</span> : null}
            </button>
          </li>
        );
      })}
    </ul>
  );
}
