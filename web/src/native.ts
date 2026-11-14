/** What the desktop app adds to the page it loads. Absent in a browser. */
export interface NativeBridge {
  notify(title: string, body?: string): Promise<void>;
  setBadge(count: number): Promise<void>;
  openWindow(url: string): Promise<void>;
}

declare global {
  interface Window {
    __pmNative?: NativeBridge;
  }
}

export function nativeBridge(): NativeBridge | null {
  return typeof window !== "undefined" ? window.__pmNative ?? null : null;
}
