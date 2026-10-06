import { useEffect, useState, type FormEvent } from "react";

const WORKSPACE_NAME_MAX_CHARS = 80;

export function normalizeWorkspaceName(name: string): string {
  return name.trim();
}

export function WorkspaceDialog({
  title,
  initialName,
  submitLabel,
  onClose,
  onSubmit,
}: {
  title: string;
  initialName: string;
  submitLabel: string;
  onClose: () => void;
  onSubmit: (name: string) => Promise<void>;
}) {
  const [name, setName] = useState(initialName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape" && !busy) onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [busy, onClose]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const normalized = normalizeWorkspaceName(name);
    if (!normalized) {
      setError("enter a workspace name");
      return;
    }
    setBusy(true);
    setError(null);
    void onSubmit(normalized).catch((reason: unknown) => {
      setError(reason instanceof Error ? reason.message : String(reason));
      setBusy(false);
    });
  };

  return (
    <div className="modal-backdrop" onClick={() => { if (!busy) onClose(); }}>
      <form className="modal workspace-name-modal" onClick={(event) => event.stopPropagation()} onSubmit={submit}>
        <h2 className="modal-title">{title}</h2>
        <label className="field">
          <span className="field-label">name</span>
          <input autoFocus value={name} maxLength={WORKSPACE_NAME_MAX_CHARS} onChange={(event) => setName(event.target.value)} placeholder="release watch" />
        </label>
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" className="btn" disabled={busy} onClick={onClose}>cancel</button>
          <button type="submit" className="btn btn-primary" disabled={busy}>{busy ? "saving…" : submitLabel}</button>
        </div>
      </form>
    </div>
  );
}
