import { forwardRef, useEffect, useImperativeHandle, useRef, type Ref } from "react";
import { Platform, StyleSheet, type NativeSyntheticEvent } from "react-native";
import { openExternalUrl } from "../adapters/openLink";
import { requireNativeViewManager } from "expo-modules-core";

import type { SocketLike } from "@puppet-master/client-core/platform";

import { TerminalViewSession } from "../../terminal-web/src/session";
import { base64ToBytes, bytesToBase64 } from "./base64";
import type { HostMessage } from "./protocol";
import type { SwiftTermSurfaceHandle, SwiftTermSurfaceProps } from "./SwiftTermSurface.types";

interface NativeTerminalRef {
  write(dataBase64: string): Promise<void>;
  focus(): Promise<void>;
  blur(): Promise<void>;
  clearSelection(): Promise<void>;
  scrollToBottom(): Promise<void>;
  revealAtBottom(): Promise<void>;
}

interface NativeTerminalProps {
  ref?: Ref<NativeTerminalRef>;
  style: object;
  fontSize: number;
  foregroundColor: string;
  backgroundColor: string;
  selectionColor: string;
  veiled: boolean;
  onInput(event: NativeSyntheticEvent<{ dataBase64: string }>): void;
  onResize(event: NativeSyntheticEvent<{ cols: number; rows: number }>): void;
  onScroll(event: NativeSyntheticEvent<{ position: number }>): void;
  onTitle(event: NativeSyntheticEvent<{ title: string }>): void;
  onLink(event: NativeSyntheticEvent<{ url: string }>): void;
}

const NativeTerminal = Platform.OS === "ios"
  ? requireNativeViewManager<NativeTerminalProps>("PuppetSwiftTerm")
  : null;

export const SwiftTermSurface = forwardRef<SwiftTermSurfaceHandle, SwiftTermSurfaceProps>(
  function SwiftTermSurface({ onMessage, veiled = false }, ref) {
    const nativeRef = useRef<NativeTerminalRef | null>(null);
    const messageRef = useRef(onMessage);
    messageRef.current = onMessage;
    const sizeRef = useRef({ cols: 80, rows: 24 });
    const localSizeRef = useRef({ cols: 80, rows: 24 });
    const sizeReceivedRef = useRef(false);
    const headerProviderRef = useRef<(() => Record<string, string>) | null>(null);
    const sizeResolveRef = useRef<((size: { cols: number; rows: number }) => void) | null>(null);
    const firstSizePromiseRef = useRef<Promise<{ cols: number; rows: number } | null> | null>(null);
    const writeChainRef = useRef(Promise.resolve());
    const sessionRef = useRef<TerminalViewSession | null>(null);

    if (!sessionRef.current) {
      const term = {
        get cols() { return sizeRef.current.cols; },
        get rows() { return sizeRef.current.rows; },
        resize(cols: number, rows: number) { sizeRef.current = { cols, rows }; },
        write(data: Uint8Array, callback?: () => void) {
          const encoded = bytesToBase64(data);
          writeChainRef.current = writeChainRef.current
            .then(async () => {
              const terminal = nativeRef.current;
              if (terminal) await terminal.write(encoded);
            })
            .then(() => callback?.())
            .catch((error: unknown) => {
              messageRef.current({
                type: "error",
                message: `SwiftTerm write failed: ${error instanceof Error ? error.message : String(error)}`,
              });
            });
        },
      };
      sessionRef.current = new TerminalViewSession({
        measureSize: () => sizeReceivedRef.current ? localSizeRef.current : null,
        post: (message) => messageRef.current(message),
        openSocket: (url, subprotocol) => {
          const headers = headerProviderRef.current?.();
          const opts = headers ? { headers } : undefined;
          return new (WebSocket as any)(url, subprotocol ? [subprotocol] : undefined, opts) as unknown as SocketLike;
        },
        term,
        now: Date.now,
      });
    }

    useImperativeHandle(ref, () => ({
      focus() {
        void nativeRef.current?.focus();
      },
      surfaceSize() {
        return sizeReceivedRef.current ? { ...localSizeRef.current } : null;
      },
      firstSize() {
        if (sizeReceivedRef.current) return Promise.resolve({ ...localSizeRef.current });
        if (!firstSizePromiseRef.current) {
          firstSizePromiseRef.current = new Promise<{ cols: number; rows: number } | null>((resolve) => {
            sizeResolveRef.current = resolve;
            setTimeout(() => {
              if (sizeResolveRef.current === resolve) {
                sizeResolveRef.current = null;
                resolve(sizeReceivedRef.current ? { ...localSizeRef.current } : null);
              }
            }, 300);
          });
        }
        return firstSizePromiseRef.current;
      },
      setHeaderProvider(provider: (() => Record<string, string>) | null) {
        headerProviderRef.current = provider;
      },
      revealAtBottom() {
        return nativeRef.current?.revealAtBottom() ?? Promise.resolve();
      },
      send(message: HostMessage) {
        switch (message.type) {
          case "scrollToBottom":
            void nativeRef.current?.scrollToBottom();
            break;
          case "refit":
          case "requestDiag":
          case "getSelection":
          case "pan":
          case "touch":
          case "cancelTouch":
          case "finishSelectionTouch":
            break;
          default:
            sessionRef.current?.handle(message);
        }
      },
    }), []);

    useEffect(() => {
      if (!NativeTerminal) return;
      messageRef.current({ type: "ready" });
      return () => sessionRef.current?.shutdown();
    }, []);

    if (!NativeTerminal) return null;

    return (
      <NativeTerminal
        ref={nativeRef}
        style={styles.terminal}
        fontSize={13}
        foregroundColor="#e8eaf0"
        backgroundColor="#080a0f"
        selectionColor="#315a91"
        veiled={veiled}
        onInput={(event) => {
          const bytes = base64ToBytes(event.nativeEvent.dataBase64);
          if (bytes) sessionRef.current?.sendInput(bytes);
        }}
        onResize={(event) => {
          const { cols, rows } = event.nativeEvent;
          if (cols < 1 || rows < 1) return;
          const changed = localSizeRef.current.cols !== cols || localSizeRef.current.rows !== rows;
          localSizeRef.current = { cols, rows };
          if (!sizeReceivedRef.current) {
            sizeReceivedRef.current = true;
            sizeResolveRef.current?.({ cols, rows });
            sizeResolveRef.current = null;
          }
          if (sessionRef.current?.shouldFit()) {
            sizeRef.current = { cols, rows };
            sessionRef.current.sendResize(cols, rows);
          }
          if (changed) messageRef.current({ type: "nativeResized" });
        }}
        onScroll={(event) => {
          messageRef.current({ type: "following", following: event.nativeEvent.position >= 0.999 });
        }}
        onTitle={(event) => messageRef.current({ type: "title", title: event.nativeEvent.title })}
        onLink={(event) => {
          const url = event.nativeEvent.url;
          messageRef.current({ type: "link", url });
          openExternalUrl(url);
        }}
      />
    );
  },
);

const styles = StyleSheet.create({
  terminal: { flex: 1 },
});
