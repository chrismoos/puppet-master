import { useEffect, useState } from "react";
import { relativeTime } from "@puppet-master/client-core/approvals";
import type { BuildInfo } from "../api/auth";
import { copyTextToClipboard } from "../clipboard";

/** The command that fetches a published build on the controller host. */
export const UPDATE_COMMAND = "pm update";

const COPIED_TIMEOUT_MS = 1500;

/**
 * What the stamp says on hover: the build this dashboard talks to, and
 * the published one when it is behind. The rev is here rather than in
 * the label, which carries the version alone.
 */
export function buildStampTitle(build: BuildInfo): string {
  const running = build.gitRev ? `${build.version} · ${build.gitRev}` : build.version;
  if (!build.updateAvailable) return `running ${running}`;
  return `running ${running} — ${build.updateAvailable} is published on ${build.channel}`;
}

/** "2 h ago", or nothing when the daemon has never read the manifest. */
export function checkedAgo(build: BuildInfo, now: number): string {
  if (build.latestCheckedAtUnixMs === null) return "";
  return relativeTime(build.latestCheckedAtUnixMs, now);
}

/** Install arrow: an update is published, and installing it is a step you take. */
function UpdateGlyph() {
  return (
    <svg className="update-glyph" viewBox="0 0 16 16" aria-hidden="true">
      <path d="M8 2.4v6.6" />
      <path d="M5.1 6.3 8 9.2l2.9-2.9" />
      <path d="M3 12.4h10" />
    </svg>
  );
}

/** The command in a shell block, with the copy its own control. */
export function UpdateCommand() {
  const [copied, setCopied] = useState(false);

  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_TIMEOUT_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  const copy = () => {
    void copyTextToClipboard(UPDATE_COMMAND)
      .then(() => setCopied(true))
      .catch(() => {});
  };

  return (
    <div className="shell-snippet">
      <code>
        <span className="shell-prompt" aria-hidden="true">$</span>
        {UPDATE_COMMAND}
      </code>
      <button type="button" className="btn btn-quiet shell-snippet-copy" onClick={copy}>
        {copied ? "copied" : "copy"}
      </button>
    </div>
  );
}

/**
 * What to say when no newer build is published. A daemon that has not
 * read its channel yet knows nothing, which is not the same as being
 * current, so it does not claim to be.
 */
export function releaseStateNote(build: BuildInfo): string {
  if (build.latestVersion === null) {
    return "This controller has not read its release channel yet, so there is nothing to compare its build against.";
  }
  return "This controller runs the newest published build.";
}

/** How a published build is installed, wherever that has to be said. */
export function UpdateInstructions() {
  return (
    <>
      <p className="update-lead">Run the following on the controller to get the latest version:</p>
      <UpdateCommand />
      <p className="update-after">After the update is fetched, restart your daemon.</p>
    </>
  );
}

export function UpdateDialog({ build, onClose }: { build: BuildInfo; onClose: () => void }) {
  const checked = checkedAgo(build, Date.now());

  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <section
        className="modal update-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="update-dialog-title"
        onClick={(event) => event.stopPropagation()}
      >
        <h2 className="modal-title" id="update-dialog-title">Update available</h2>
        <p className="update-versions">
          <span className="from">{build.version}</span>
          <span className="arrow" aria-hidden="true">→</span>
          <span className="to">{build.updateAvailable}</span>
        </p>
        <p className="update-meta">
          channel <code>{build.channel}</code>
          {checked && ` · checked ${checked}`}
        </p>
        <UpdateInstructions />
        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>Close</button>
        </div>
      </section>
    </div>
  );
}

/**
 * The build this dashboard is talking to. It becomes a button only when
 * a newer build is published, so there is nothing to click the rest of
 * the time.
 */
export function BuildStamp({ build }: { build: BuildInfo | null }) {
  const [open, setOpen] = useState(false);
  if (!build) return null;

  if (!build.updateAvailable) {
    return (
      <span className="build-stamp" title={buildStampTitle(build)}>
        v{build.version}
      </span>
    );
  }

  return (
    <>
      <button
        type="button"
        className="build-stamp update-stamp"
        title={buildStampTitle(build)}
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <UpdateGlyph />
        v{build.version}
      </button>
      {open && <UpdateDialog build={build} onClose={() => setOpen(false)} />}
    </>
  );
}
