import { useState, type FormEvent } from "react";
import { login } from "../api/auth";
import { ProductBrand } from "../components/ProductBrand";

export function LoginPage({ onDone }: { onDone: (username: string) => void }) {
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    login(username, password)
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
        <p className="auth-sub">log in to your daemon</p>
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
            autoComplete="current-password"
            required
          />
        </label>
        {error && <p className="form-error">{error}</p>}
        <button type="submit" className="btn btn-primary btn-wide" disabled={busy}>
          {busy ? "logging in…" : "log in"}
        </button>
      </form>
    </div>
  );
}
