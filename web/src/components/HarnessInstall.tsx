import { useEffect, useRef, useState } from "react";
import { agentFromValue, agentLabel } from "@puppet-master/client-core/state/agent";
import type { HarnessReply } from "../api/harness";
import { prepareHarness } from "../state/harnessInstall";
import type { SpawnRequest } from "../state/spawnSubmit";

export function HarnessInstall({ reply, checking, confirm }: {
  reply: HarnessReply | null;
  checking: boolean;
  confirm?: (accepted: boolean) => void;
}) {
  if (checking) return <div className="harness-install" role="status"><progress aria-label="Checking harness" /> Checking harness on worker…</div>;
  if (!reply || reply.status.state === "ready" || reply.status.state === "unsupported") return null;
  const agent = agentFromValue(reply.agent);
  const name = reply.agent === "claude" ? "Claude Code" : agent === undefined ? reply.agent : agentLabel(agent);
  const { status } = reply;
  return <section className="harness-install" aria-label="Harness installation">
    {confirm ? <>
      <p>{name} is not installed on this worker. Install it?</p>
      <button type="button" className="btn btn-primary" onClick={() => confirm(true)}>Install</button>{" "}
      <button type="button" className="btn" onClick={() => confirm(false)}>Cancel</button>
    </> : status.state === "installing" ? <div role="status">
      <progress aria-label={`Installing ${name}`} /> Installing {name} on worker…
    </div> : null}
    {status.error && <p role="alert">Installation failed: {status.error}</p>}
    {status.output && <pre aria-label="Installer output">{status.output}</pre>}
  </section>;
}

export function useHarnessInstall() {
  const [reply, setReply] = useState<HarnessReply | null>(null);
  const [checking, setChecking] = useState(false);
  const [confirm, setConfirm] = useState<((accepted: boolean) => void) | undefined>();
  const pending = useRef<((accepted: boolean) => void) | undefined>(undefined);
  const active = useRef<AbortController | null>(null);
  useEffect(() => () => {
    active.current?.abort();
    pending.current?.(false);
  }, []);
  const prepare = async (request: SpawnRequest) => {
    const controller = new AbortController();
    active.current = controller;
    setReply(null);
    setChecking(true);
    try {
      return await prepareHarness(request, controller.signal, (next) => {
        setChecking(false);
        setReply(next);
      }, () => new Promise<boolean>((resolve) => {
        const answer = (accepted: boolean) => {
          pending.current = undefined;
          setConfirm(undefined);
          if (!accepted) setReply(null);
          else setReply((current) => current && { ...current, status: { ...current.status, state: "installing", error: "", output: "" } });
          resolve(accepted);
        };
        pending.current = answer;
        setConfirm(() => answer);
      }));
    } catch (error) {
      setReply((current) => current?.status.state === "installing"
        ? { ...current, status: { ...current.status, state: "failed", error: error instanceof Error ? error.message : String(error) } }
        : current);
      throw error;
    } finally {
      setChecking(false);
      active.current = null;
    }
  };
  return { prepare, panel: <HarnessInstall reply={reply} checking={checking} confirm={confirm} /> };
}
