import { useState } from "react";
import { formatArguments, formatSize, type ArgumentsSummary } from "@puppet-master/client-core/approvals";
import { downloadCallArguments } from "../api/approvals";
import "./CallArguments.css";

/** A call's arguments read whole, or for a large set its source, digest, outline and first lines. */
export function CallArguments({
  call,
}: {
  call: { id: string; arguments: unknown; arguments_path?: string; arguments_summary?: ArgumentsSummary };
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const [failure, setFailure] = useState("");
  const summary = call.arguments_summary;
  const source = call.arguments_path && (
    <span>
      <span className="call-args-dim">From</span> <code>{call.arguments_path}</code>
    </span>
  );
  if (!summary)
    return (
      <>
        {source && <div className="call-args-source">{source}</div>}
        <pre>{formatArguments(call.arguments)}</pre>
      </>
    );
  const copy = () => {
    void navigator.clipboard?.writeText(summary.sha256).then(() => setCopied(true));
  };
  const download = () => {
    setFailure("");
    downloadCallArguments(call.id).catch((error: unknown) =>
      setFailure(error instanceof Error && error.message ? error.message : "Download failed"),
    );
  };
  return (
    <div className="call-args">
      <div className="call-args-source">
        {source}
        <span>
          <span className="call-args-dim">Size</span> {formatSize(summary.bytes)}
        </span>
        <span className="call-args-hash" title={`SHA-256 of the exact arguments: ${summary.sha256}`}>
          sha256 {summary.sha256.slice(0, 6)}…{summary.sha256.slice(-4)}
          <button type="button" onClick={copy}>
            {copied ? "Copied" : "Copy"}
          </button>
        </span>
      </div>
      {summary.outline.length > 0 && (
        <dl className="call-args-outline" aria-label="Outline">
          {summary.outline.map(({ field, shape }) => (
            <div key={field} className="call-args-field">
              <dt>{field || "arguments"}</dt>
              <dd>{shape}</dd>
            </div>
          ))}
        </dl>
      )}
      <div className={expanded ? "call-args-preview" : "call-args-preview is-clipped"}>
        <pre>{expanded ? formatArguments(call.arguments) : summary.preview}</pre>
      </div>
      <div className="call-args-actions">
        <button type="button" className="btn" onClick={() => setExpanded(!expanded)}>
          {expanded ? "Show less" : "Show all"}
        </button>
        <button type="button" className="btn" onClick={download}>
          Download JSON
        </button>
        <span className="call-args-dim">
          {expanded
            ? `All ${summary.lines.toLocaleString()} lines`
            : `Showing the first ${summary.preview.split("\n").length} of ${summary.lines.toLocaleString()} lines`}
        </span>
      </div>
      {failure && (
        <p className="call-args-error" role="alert">
          {failure}
        </p>
      )}
    </div>
  );
}
