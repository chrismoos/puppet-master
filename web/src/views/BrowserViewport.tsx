import { useEffect, useRef, useState } from "react";

const RECONNECT_MS = 1500;
const MAX_RECONNECTS = 3;

type BrowserStatus = { state: string; url?: string; action?: string; width?: number; height?: number };

export function BrowserViewport({ path, active, onReady }: { path: string; active: boolean; onReady: () => void }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  const ready = useRef(onReady);
  ready.current = onReady;
  const [status, setStatus] = useState<BrowserStatus>({ state: "connecting" });
  const [retry, setRetry] = useState(0);
  const [pageVisible, setPageVisible] = useState(() => document.visibilityState !== "hidden");
  const watching = active && pageVisible;
  useEffect(() => {
    const changed = () => setPageVisible(document.visibilityState !== "hidden");
    document.addEventListener("visibilitychange", changed);
    return () => document.removeEventListener("visibilitychange", changed);
  }, []);
  useEffect(() => {
    if (!watching || !path) return;
    let disposed = false;
    let socket: WebSocket | undefined;
    let timer: number | undefined;
    let attempts = 0;
    let pending: ArrayBuffer | null = null;
    let drawing = false;
    let receivedFrame = false;
    async function paint() {
      if (drawing) return;
      drawing = true;
      try {
        while (pending && !disposed) {
          const data = pending;
          pending = null;
          const bitmap = await createImageBitmap(new Blob([data], { type: "image/jpeg" }));
          try {
            const element = canvas.current;
            if (disposed || !element) return;
            if (element.width !== bitmap.width) element.width = bitmap.width;
            if (element.height !== bitmap.height) element.height = bitmap.height;
            element.getContext("2d")?.drawImage(bitmap, 0, 0);
            receivedFrame = true;
            ready.current();
          } finally { bitmap.close(); }
        }
      } catch {
        if (!disposed) setStatus(previous => ({ ...previous, state: "disconnected" }));
      } finally { drawing = false; }
    }
    function connect() {
      setStatus(previous => ({ ...previous, state: "connecting" }));
      const url = new URL(path, document.baseURI);
      url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
      const connection = socket = new WebSocket(url);
      connection.binaryType = "arraybuffer";
      connection.onmessage = event => {
        if (disposed || socket !== connection) return;
        attempts = 0;
        if (typeof event.data === "string") {
          try { setStatus(JSON.parse(event.data) as BrowserStatus); } catch { setStatus(previous => ({ ...previous, state: "disconnected" })); }
        } else if (event.data instanceof ArrayBuffer) {
          pending = event.data;
          void paint();
        }
      };
      connection.onclose = event => {
        if (disposed || socket !== connection) return;
        setStatus(previous => ({ ...previous, state: "disconnected" }));
        if (event.code !== 1000 && attempts++ < MAX_RECONNECTS) timer = window.setTimeout(connect, RECONNECT_MS);
      };
      connection.onerror = () => connection.close();
    }
    connect();
    return () => {
      disposed = true;
      pending = null;
      window.clearTimeout(timer);
      socket?.close();
      if (!receivedFrame) ready.current();
    };
  }, [path, watching, retry]);
  const state = watching ? status.state : "paused";
  return (
    <div className="browser-viewport">
      <div className="browser-location">
        <span className="browser-url" title={status.url}>{status.url || "Managed browser"}</span>
        <span className="browser-connection" role="status">{state === "live" ? "Live · read-only" : state === "connecting" ? "Connecting…" : state === "paused" ? "Paused" : state === "empty" ? "No open tab" : "Disconnected"}</span>
      </div>
      <div className="browser-screen">
        <canvas ref={canvas} role="img" aria-label="Live agent browser" />
        {state === "disconnected" && <div className="browser-disconnected"><p>Browser stream disconnected.</p><p>The agent can show it again if the browser has closed.</p><button type="button" onClick={() => setRetry(value => value + 1)}>Reconnect</button></div>}
      </div>
      <div className="browser-caption"><span>{status.action || "Waiting for the agent"}</span><span>{status.width && status.height ? `${status.width} × ${status.height}` : ""}</span></div>
    </div>
  );
}
