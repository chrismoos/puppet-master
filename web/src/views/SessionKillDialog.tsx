import { useEffect, useState } from "react";

export function SessionKillDialog({
  sessionName,
  onClose,
  onConfirm,
}: {
  sessionName: string;
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
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="session-kill-title"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="modal-title" id="session-kill-title">kill session</h2>
        <p>Kill “{sessionName}”?</p>
        <p className="muted-line">
          This stops the agent process and ends the session. The session record remains available,
          and a saved agent conversation can be resumed later.
        </p>
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" className="btn" disabled={busy} onClick={onClose}>cancel</button>
          <button type="button" className="btn btn-danger" disabled={busy} onClick={confirm}>
            {busy ? "killing…" : "kill session"}
          </button>
        </div>
      </section>
    </div>
  );
}
