import type { ReactNode } from "react";

export const SETTINGS_PAGE_TITLE_ID = "settings-page-title";

/** The one title a Settings page has, with its main action beside it. */
export function SettingsPageHead({
  title,
  description,
  actions,
}: {
  title: string;
  description: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="set-page-head">
      <div>
        <h2 id={SETTINGS_PAGE_TITLE_ID} tabIndex={-1}>{title}</h2>
        <p>{description}</p>
      </div>
      {actions && <div className="set-page-actions">{actions}</div>}
    </header>
  );
}

export function SettingsError({ message, onDismiss }: { message: string | null; onDismiss: () => void }) {
  if (!message) return null;
  return (
    <div className="flash-error set-flash" role="alert">
      <span>{message}</span>
      <button type="button" className="btn" onClick={onDismiss}>Dismiss</button>
    </div>
  );
}

export function errorMessage(reason: unknown): string {
  return reason instanceof Error ? reason.message : String(reason);
}
