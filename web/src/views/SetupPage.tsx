import { useState, type FormEvent } from "react";
import { setup } from "../api/auth";
import { ProductBrand } from "../components/ProductBrand";

export function SetupPage({ onDone }: { onDone: (username: string) => void }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (password !== confirm) {
      setError("passwords do not match");
      return;
    }
    setBusy(true);
    setError(null);
    setup(username, password)
      .then(() => onDone(username))
      .catch((err: unknown) => {
        setError(err instanceof Error ? err.message : String(err));
        setBusy(false);
      });
  };

  return (
    <div className="auth-screen">
      <form className="auth-card" onSubmit={submit}>
        <ProductBrand />
        <p className="auth-sub">first run — create the user for this daemon</p>
        <label className="field">
          <span className="field-label">username</span>
          <input
            autoFocus
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            autoComplete="username"
            required
          />
        </label>
        <label className="field">
          <span className="field-label">password</span>
          <input
            type="password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            autoComplete="new-password"
            required
          />
        </label>
        <label className="field">
          <span className="field-label">confirm password</span>
          <input
            type="password"
            value={confirm}
            onChange={(e) => setConfirm(e.target.value)}
            autoComplete="new-password"
            required
          />
        </label>
        {error && <p className="form-error">{error}</p>}
        <button type="submit" className="btn btn-primary btn-wide" disabled={busy}>
          {busy ? "creating…" : "create user"}
        </button>
      </form>
    </div>
  );
}
