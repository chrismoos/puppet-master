import type { ReactNode } from "react";
import { AGENTS } from "@puppet-master/client-core/state/agent";

/**
 * The agent picker shared by every default-agent and spawn-override
 * control. Its options come from the shared agent table, so adding an
 * agent reaches all of them at once. `inheritLabel` names the empty
 * value, which means "no explicit choice" everywhere it is offered.
 */
export function AgentSelect({
  value,
  onChange,
  inheritLabel,
}: {
  value: string;
  onChange: (value: string) => void;
  inheritLabel: ReactNode;
}) {
  return (
    <select value={value} onChange={(event) => onChange(event.target.value)}>
      <option value="">{inheritLabel}</option>
      {AGENTS.map((agent) => (
        <option key={agent.value} value={agent.value}>{agent.label}</option>
      ))}
    </select>
  );
}
