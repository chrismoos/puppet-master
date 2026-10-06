import { useCallback, useEffect, useMemo, useState } from "react";
import { InstructionTarget, SessionRole } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { useAppState, useClient } from "../state/hooks";
import { SettingsPageHead } from "./settingsParts";

type History = { revision: number; markdown: string; note: string; updated_at_unix_ms: number };
type Selection = { bucket: string; project: string; target: InstructionTarget };
type EditorTab = "write" | "worker" | "supervisor";

const TARGETS: ReadonlyArray<{ value: InstructionTarget; label: string }> = [
  { value: InstructionTarget.ALL, label: "Everyone" },
  { value: InstructionTarget.WORKER, label: "Workers" },
  { value: InstructionTarget.SUPERVISOR, label: "Supervisors" },
];

const TABS: ReadonlyArray<{ id: EditorTab; label: string; role?: SessionRole }> = [
  { id: "write", label: "Write" },
  { id: "worker", label: "Preview as worker", role: SessionRole.WORKER },
  { id: "supervisor", label: "Preview as supervisor", role: SessionRole.SUPERVISOR },
];

export function InstructionsPanel() {
  const state = useAppState(), client = useClient();
  const buckets = [...state.buckets.values()].sort((a, b) => a.name.localeCompare(b.name));
  const [selection, setSelection] = useState<Selection>(() => ({ bucket: buckets[0]?.id.toString() ?? "", project: "", target: InstructionTarget.ALL }));
  const [pending, setPending] = useState<Selection | null>(null);
  const [markdown, setMarkdown] = useState(""), [note, setNote] = useState("");
  const [loadedMarkdown, setLoadedMarkdown] = useState("");
  const [tab, setTab] = useState<EditorTab>("write");
  const [preview, setPreview] = useState(""), [history, setHistory] = useState<History[]>([]);
  const [error, setError] = useState("");
  const layers = useMemo(() => [...state.instructionLayers.values()].filter((l) => l.bucketId.toString() === selection.bucket && (selection.project ? l.projectId?.toString() === selection.project : l.projectId === undefined)), [state.instructionLayers, selection.bucket, selection.project]);
  const current = layers.find((l) => l.target === selection.target);
  const projects = [...state.projects.values()].filter((p) => p.bucketId.toString() === selection.bucket);
  const dirty = markdown !== loadedMarkdown || note.trim() !== "";

  const load = useCallback(() => {
    const value = current?.markdown ?? "";
    setMarkdown(value); setLoadedMarkdown(value); setNote(""); setPreview(""); setTab("write"); setError("");
    if (!selection.bucket || !current) { setHistory([]); return; }
    client.listInstructions(BigInt(selection.bucket), selection.project ? BigInt(selection.project) : undefined).then((o) => {
      const parsed = JSON.parse(new TextDecoder().decode(o.data ?? new Uint8Array())) as Array<{ id: number; history: History[] }>;
      setHistory(parsed.find((x) => BigInt(x.id) === current.id)?.history ?? []);
    }).catch((e: unknown) => setError(String(e)));
  }, [client, current, selection.bucket, selection.project]);

  useEffect(load, [load]);
  const choose = (next: Selection) => dirty ? setPending(next) : setSelection(next);
  const save = () => client.setInstructions(BigInt(selection.bucket), selection.project ? BigInt(selection.project) : undefined, selection.target, markdown, current?.revision ?? 0n, note).then(() => { setLoadedMarkdown(markdown); setNote(""); }).catch((e: unknown) => setError(String(e)));
  const showPreview = (role: SessionRole) => client.getEffectiveInstructions(BigInt(selection.bucket), role, selection.project ? BigInt(selection.project) : undefined).then((o) => setPreview(new TextDecoder().decode(o.data ?? new Uint8Array()))).catch((e: unknown) => setError(String(e)));
  const openTab = (next: typeof TABS[number]) => {
    setTab(next.id);
    setPreview("");
    if (next.role !== undefined) void showPreview(next.role);
  };

  return (
    <section className="set-page instructions-panel" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Instructions"
        description="Extra instructions layered onto agents at spawn and resume, never mid-turn. An agent's own CLAUDE.md and AGENTS.md are left alone."
      />
      <div className="ui-form-card set-editor">
        <div className="set-layer-bar">
          <span className="lbl">Layer for</span>
          <select className="ui-select" aria-label="Bucket" value={selection.bucket} onChange={(e) => choose({ bucket: e.target.value, project: "", target: selection.target })}>
            {buckets.map((b) => <option key={b.id.toString()} value={b.id.toString()}>{b.name}</option>)}
          </select>
          <span className="lbl" aria-hidden="true">›</span>
          <select className="ui-select" aria-label="Scope" value={selection.project} onChange={(e) => choose({ ...selection, project: e.target.value })}>
            <option value="">Whole bucket</option>
            {projects.map((p) => <option key={p.id.toString()} value={p.id.toString()}>Project: {p.name}</option>)}
          </select>
          <span className="lbl applies">Applies to</span>
          <div className="ui-seg" role="group" aria-label="Applies to">
            {TARGETS.map((target) => (
              <button key={target.value} type="button" aria-pressed={selection.target === target.value} onClick={() => choose({ ...selection, target: target.value })}>
                {target.label}
              </button>
            ))}
          </div>
        </div>
        <div className="set-tabs" role="tablist" aria-label="Editor view">
          {TABS.map((entry) => (
            <button key={entry.id} type="button" role="tab" id={`instructions-tab-${entry.id}`} aria-selected={tab === entry.id} aria-controls="instructions-tabpanel" onClick={() => openTab(entry)}>
              {entry.label}
            </button>
          ))}
          <span className="meta">
            Markdown · {current ? `revision ${current.revision.toString()}` : "new layer, not saved yet"}
          </span>
        </div>
        <div id="instructions-tabpanel" role="tabpanel" aria-labelledby={`instructions-tab-${tab}`}>
          {tab === "write" ? (
            <textarea className="ui-textarea" rows={13} aria-label="Instructions" value={markdown} onChange={(e) => setMarkdown(e.target.value)} placeholder={"# Delivery rules\n\nA worker commit on a task branch is a checkpoint, not delivered work.\nRun the focused checks before you report done."} />
          ) : (
            <pre className="instruction-preview">{preview || "No instructions apply to this role here."}</pre>
          )}
        </div>
        <footer>
          <input className="ui-input" aria-label="Revision note" placeholder="Revision note, for the history below (optional)" value={note} onChange={(e) => setNote(e.target.value)} />
          <button className="btn btn-primary btn-lg" type="button" onClick={save} disabled={!dirty}>Save revision</button>
        </footer>
      </div>
      {error && <p className="form-error" role="alert">{error}</p>}
      <div className="ui-sect">
        <div className="ui-sect-head">
          <h3>History</h3>
          {history.length === 0 && <span>Saved revisions of this layer appear here, each with a revert.</span>}
        </div>
        {history.length > 0 && (
          <div className="ui-list">
            {history.map((h) => (
              <div key={h.revision} className="ui-row instruction-revision">
                <code className="ui-key">r{h.revision}</code>
                <div className="main">
                  <div className="title">{h.note || "No note"}</div>
                  <div className="desc">{new Date(h.updated_at_unix_ms).toLocaleString()}</div>
                </div>
                <div className="ctl">
                  <button type="button" className="btn btn-quiet" aria-label={`Revert to revision ${h.revision}`} onClick={() => current && client.revertInstructions(current.id, BigInt(h.revision), current.revision, `revert to revision ${h.revision}`).catch((e: unknown) => setError(String(e)))}>Revert</button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
      {pending && <div className="modal-backdrop"><div className="modal-card" role="dialog" aria-modal="true" aria-labelledby="discard-instructions-title"><h2 id="discard-instructions-title">Discard unsaved changes?</h2><p>You have unsaved instruction edits. Discard them and load the selected layer?</p><div className="modal-actions"><button className="btn" type="button" onClick={() => setPending(null)}>Keep editing</button><button className="btn btn-danger" type="button" onClick={() => { setSelection(pending); setPending(null); }}>Discard and load</button></div></div></div>}
    </section>
  );
}
