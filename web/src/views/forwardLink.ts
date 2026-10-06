import { openableForwardUrl } from "@puppet-master/client-core/api/forwards";
import type { JsonFetch } from "@puppet-master/client-core/platform";

/** A window opened before the mint so the navigation is not popup-blocked. */
export interface OpenedWindow {
  navigate(url: string): void;
  close(): void;
}

export type WindowOpener = () => OpenedWindow | null;

export function browserWindowOpener(): OpenedWindow | null {
  const win = window.open("", "_blank");
  if (!win) return null;
  win.opener = null;
  return {
    navigate: (url) => {
      win.location.href = url;
    },
    close: () => win.close(),
  };
}

/**
 * Opens a published forward in a new tab with a freshly minted scoped token.
 * The tab is opened synchronously from the click, then pointed at the tokened
 * URL once the mint resolves, and closed again if the mint fails.
 */
export async function openForward(
  fetchImpl: JsonFetch,
  openWindow: WindowOpener,
  forwardId: string,
  url: string,
  validateDestination = false,
): Promise<void> {
  const win = openWindow();
  if (!win) throw new Error("the browser blocked opening a new tab");
  try {
    win.navigate(await openableForwardUrl(fetchImpl, forwardId, url, validateDestination));
  } catch (err) {
    win.close();
    throw err;
  }
}

export function parseForwardHandoff(hash: string): { id: string; destination: string } | null {
  const prefix = "#/forward-open?";
  if (!hash.startsWith(prefix)) return null;
  const query = new URLSearchParams(hash.slice(prefix.length));
  const id = query.get("id");
  const destination = query.get("destination");
  if (!id || !/^[1-9][0-9]*$/.test(id) || !destination) throw new Error("Invalid forward destination");
  return { id, destination };
}
