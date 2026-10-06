import type { SessionState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { stateStyle } from "@puppet-master/client-core/format";

/**
 * The session state as a labeled pill, or — with `dot` — a compact
 * colored dot that carries the label as its accessible name and tooltip,
 * for space-tight rows.
 */
export function StateBadge({ state, dot = false, seen = false }: { state: SessionState; dot?: boolean; seen?: boolean }) {
  const style = stateStyle(state);
  if (dot) {
    return (
      <span
        className={`state-dot ${style.className}${seen ? " is-attention-seen" : ""}`}
        role="img"
        aria-label={style.label}
        title={style.label}
      />
    );
  }
  return <span className={`badge ${style.className}`}>{style.label}</span>;
}
