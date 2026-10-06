import * as core from "@puppet-master/client-core/api/items";
import type { AbortSignalLike } from "@puppet-master/client-core/platform";
import { browserJsonFetch } from "./http";
import { accessToken } from "./token";

export {
  attachmentDownloadUrl,
  type BriefingEntry,
  type ItemAttachment,
  type ItemNote,
  type ItemNoteKind,
  type ItemSearchCounts,
  type ItemSearchFilters,
  type ItemSearchPage,
} from "@puppet-master/client-core/api/items";

export const fetchItemNotes = (bucketId: string, itemId: string, signal?: AbortSignalLike) =>
  core.fetchItemNotes(browserJsonFetch, bucketId, itemId, signal);

export const fetchItemAttachments = (bucketId: string, itemId: string, signal?: AbortSignalLike) =>
  core.fetchItemAttachments(browserJsonFetch, bucketId, itemId, signal);

export const deleteItemAttachment = (bucketId: string, itemId: string, attachmentId: string) =>
  core.deleteItemAttachment(browserJsonFetch, bucketId, itemId, attachmentId);

export const fetchItem = (bucketId: string, itemId: string, signal?: AbortSignalLike) =>
  core.fetchItem(browserJsonFetch, bucketId, itemId, signal);

export const fetchItems = (
  bucketId: string,
  filters: core.ItemSearchFilters,
  offset = 0,
  signal?: AbortSignalLike,
) => core.fetchItems(browserJsonFetch, bucketId, filters, offset, signal);

export const fetchItemWindow = (
  bucketId: string,
  filters: core.ItemSearchFilters,
  size: number,
  signal?: AbortSignalLike,
) => core.fetchItemWindow(browserJsonFetch, bucketId, filters, size, signal);

export const fetchBriefings = (bucketId: string, signal?: AbortSignalLike) =>
  core.fetchBriefings(browserJsonFetch, bucketId, signal);

/// Uploads through `XMLHttpRequest` rather than `fetch`, because it is the only
/// way to report progress, which means it carries the access token itself.
export async function uploadItemAttachment(
  bucketId: string,
  itemId: string,
  file: File,
  onProgress: (percent: number) => void,
): Promise<core.ItemAttachment> {
  const bearer = await accessToken();
  return new Promise((resolve, reject) => {
    const request = new XMLHttpRequest();
    const mediaType = file.type || "application/octet-stream";
    request.open("POST", core.attachmentUploadPath(bucketId, itemId, file.name, mediaType));
    request.setRequestHeader("Content-Type", mediaType);
    request.setRequestHeader("Authorization", `Bearer ${bearer}`);
    request.upload.addEventListener("progress", (event) => {
      if (event.lengthComputable) onProgress(Math.round((event.loaded / event.total) * 100));
    });
    request.addEventListener("load", () => {
      let body: { attachment?: core.ItemAttachment; error?: string } = {};
      try { body = JSON.parse(request.responseText) as typeof body; } catch { /* handled below */ }
      if (request.status >= 200 && request.status < 300 && body.attachment) {
        onProgress(100); resolve(body.attachment);
      } else reject(new Error(body.error || `attachment upload failed (${request.status})`));
    });
    request.addEventListener("error", () => reject(new Error("attachment upload interrupted")));
    request.addEventListener("abort", () => reject(new Error("attachment upload interrupted")));
    request.send(file);
  });
}
