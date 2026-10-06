import { useState } from "react";
import { copyTextToClipboard } from "../clipboard";

export type CopyResult = "copied" | "failed";

export async function copyItemReference(reference: string): Promise<CopyResult> {
  try {
    await copyTextToClipboard(reference);
    return "copied";
  } catch {
    return "failed";
  }
}

export function ItemReference({ bucketId, id, onOpen }: {
  bucketId: string;
  id: string;
  onOpen: () => void;
}) {
  const reference = `pm:item/${bucketId}/${id}`;
  const [copyResult, setCopyResult] = useState<CopyResult | null>(null);

  const copy = async () => setCopyResult(await copyItemReference(reference));

  return <span className="item-reference">
    <a href={`#/bucket/${bucketId}/item/${id}`} onClick={(event) => { event.preventDefault(); onOpen(); }}>{reference}</a>
    <button
      className="item-reference-copy"
      type="button"
      aria-label={`Copy item reference ${reference}`}
      title="Copy item reference"
      onClick={() => void copy()}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M5.5 4.5V3.25c0-.97.78-1.75 1.75-1.75h5.5c.97 0 1.75.78 1.75 1.75v5.5c0 .97-.78 1.75-1.75 1.75H11.5v1.25c0 .97-.78 1.75-1.75 1.75h-5.5c-.97 0-1.75-.78-1.75-1.75v-5.5c0-.97.78-1.75 1.75-1.75H5.5Zm1.5 0h2.75c.97 0 1.75.78 1.75 1.75V9h1.25c.14 0 .25-.11.25-.25v-5.5a.25.25 0 0 0-.25-.25h-5.5a.25.25 0 0 0-.25.25V4.5Zm-2.75 1.5a.25.25 0 0 0-.25.25v5.5c0 .14.11.25.25.25h5.5c.14 0 .25-.11.25-.25v-5.5a.25.25 0 0 0-.25-.25h-5.5Z" />
      </svg>
    </button>
    {copyResult && <span className={`item-reference-feedback is-${copyResult}`} role="status">
      {copyResult === "copied" ? "Copied" : "Copy failed"}
    </span>}
  </span>;
}
