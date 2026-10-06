import type { UpdateChanges } from "../api/connections";
import { updateFieldChanges } from "./connectionsModel";
import { Badge, useNames } from "./connectionsParts";

/** What an agent's update proposal changes about a connection besides its policy. */
export function ConnectionUpdateReview({ changes }: { changes: UpdateChanges }) {
  const { projectName } = useNames();
  const fields = updateFieldChanges(changes, projectName);
  const { added, removed, changed } = changes.tools;
  return (
    <div className="cx-update" aria-label="Proposed connection changes">
      {changes.requires_activation && (
        <p className="cx-update-warn" role="note">
          Applying deactivates this connection. Its stored credential may be cleared, and it must
          be tested and activated again before agents can use it.
        </p>
      )}
      {fields.length > 0 && (
        <dl className="cx-update-fields">
          {fields.map((change) => (
            <div key={change.label}>
              <dt>{change.label}</dt>
              <dd>
                {change.to === undefined ? (
                  "Replaced"
                ) : (
                  <>
                    <del>{change.from}</del> <ins>{change.to}</ins>
                  </>
                )}
              </dd>
            </div>
          ))}
        </dl>
      )}
      {added.length + removed.length + changed.length > 0 && (
        <ul className="cx-update-tools" aria-label="Proposed tool changes">
          {added.map((tool) => (
            <li key={tool.name}>
              <Badge tone="ok">Added</Badge> <span className="cx-name">{tool.name}</span>{" "}
              <small className="cx-dim">{tool.description}</small>
            </li>
          ))}
          {changed.map((name) => (
            <li key={name}>
              <Badge tone="pending">Changed</Badge> <span className="cx-name">{name}</span>
            </li>
          ))}
          {removed.map((name) => (
            <li key={name}>
              <Badge tone="bad">Removed</Badge> <span className="cx-name">{name}</span>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
