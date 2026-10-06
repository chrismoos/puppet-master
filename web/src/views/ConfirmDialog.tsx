import { useEffect, useState, type ReactNode } from "react";

/** A modal that asks before an action the user cannot take back. */
export function ConfirmDialog({
  title,
  titleId,
  className,
  confirmLabel,
  busyLabel,
  children,
  onClose,
  onConfirm,
}: {
  title: string;
  titleId: string;
  className?: string;
  confirmLabel: string;
  busyLabel: string;
  children: ReactNode;
  onClose: () => void;
  onConfirm: () => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  const confirm = () => {
    setBusy(true);
    setError(null);
    void onConfirm().catch((reason: unknown) => {
      setError(reason instanceof Error ? reason.message : String(reason));
      setBusy(false);
    });
  };

  return (
    <div className="modal-backdrop" onClick={() => { if (!busy) onClose(); }}>
      <section
        className={`modal confirm-modal${className ? ` ${className}` : ""}`}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="modal-title" id={titleId}>{title}</h2>
        {children}
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" className="btn" disabled={busy} onClick={onClose}>cancel</button>
          <button type="button" className="btn btn-danger" disabled={busy} onClick={confirm}>{busy ? busyLabel : confirmLabel}</button>
        </div>
      </section>
    </div>
  );
}
