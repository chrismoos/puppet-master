import { UI_THEMES, uiThemeLabel, type UiTheme } from "@puppet-master/client-core/theme/uiTheme";

export interface UiThemeChoice {
  key: UiTheme;
  name: string;
  typeface: string;
}

const UI_THEME_TYPEFACES: Readonly<Record<UiTheme, string>> = {
  standard: "JetBrains Mono",
  graphite: "Inter",
  studio: "Inter",
};

export const UI_THEME_CHOICES: UiThemeChoice[] = UI_THEMES.map((key) => ({
  key,
  name: uiThemeLabel(key),
  typeface: UI_THEME_TYPEFACES[key],
}));

const SWATCH_ACCENTS = ["--amber", "--green", "--red"];

export function UiThemePicker({
  selected,
  disabled = false,
  onSelect,
}: {
  selected: UiTheme;
  disabled?: boolean;
  onSelect: (theme: UiTheme) => void;
}) {
  return (
    <ul className="theme-picker ui-theme-picker" aria-label="Application theme">
      {UI_THEME_CHOICES.map((choice) => {
        const isSelected = choice.key === selected;
        return (
          <li key={choice.key}>
            <button
              type="button"
              className={`theme-card${isSelected ? " is-selected" : ""}`}
              aria-pressed={isSelected}
              disabled={disabled}
              onClick={() => onSelect(choice.key)}
            >
              {/* data-theme scopes that theme's tokens to the swatch, so each card previews itself. */}
              <span className="theme-card-swatch ui-theme-swatch" data-theme={choice.key} aria-hidden="true">
                <span className="ui-theme-swatch-head">
                  <span className="theme-card-sample">Aa</span>
                  <span className="ui-theme-swatch-face">{choice.typeface}</span>
                </span>
                <span className="ui-theme-swatch-foot">
                  <span className="ui-theme-swatch-control">Apply</span>
                  {SWATCH_ACCENTS.map((accent) => (
                    <span key={accent} className="ui-theme-swatch-accent" style={{ background: `var(${accent})` }} />
                  ))}
                </span>
              </span>
              <span className="theme-card-name">{choice.name}</span>
              <span className="theme-card-meta">
                {choice.typeface}
                {isSelected ? <em>active</em> : null}
              </span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}
