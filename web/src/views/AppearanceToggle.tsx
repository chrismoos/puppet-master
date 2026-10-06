import { useState } from "react";
import {
  appearanceLabel,
  nextAppearance,
  type Appearance,
} from "@puppet-master/client-core/theme/appearance";
import { applyAppearance, resetAppearance } from "../api/userSettings";

const GLYPHS: Record<string, string> = {
  system: "M8 1.5a6.5 6.5 0 1 0 0 13Z",
  light: "M8 4.5a3.5 3.5 0 1 0 0 7 3.5 3.5 0 0 0 0-7ZM8 0v2M8 14v2M0 8h2M14 8h2M2.3 2.3l1.4 1.4M12.3 12.3l1.4 1.4M13.7 2.3l-1.4 1.4M3.7 12.3l-1.4 1.4",
  dark: "M13 10.2A5.6 5.6 0 0 1 5.8 3a5.7 5.7 0 1 0 7.2 7.2Z",
};

/// One control cycling system, light, dark. Following the system is a
/// state the user can return to, so it is in the cycle rather than being
/// something only a fresh account gets.
export function AppearanceToggle({ appearance }: { appearance: Appearance | null }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const current = appearanceLabel(appearance);
  const next = nextAppearance(appearance);

  const choose = async () => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      if (next === null) await resetAppearance();
      else await applyAppearance(next);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  return (
    <button
      type="button"
      className="btn topbar-icon appearance-toggle"
      data-appearance={current}
      disabled={busy}
      title={error ?? `appearance: ${current} (switch to ${appearanceLabel(next)})`}
      aria-label={`Appearance: ${current}. Switch to ${appearanceLabel(next)}.`}
      onClick={() => void choose()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d={GLYPHS[current]} />
      </svg>
    </button>
  );
}
