// Host-universal APIs shared by browsers, Node, and React Native (Hermes).
// This package compiles against lib ES2022 only, so anything beyond the
// ECMAScript standard library must be declared here or injected through
// the adapter types in src/platform.ts. Keep this list minimal: adding a
// browser-only API here defeats the platform-neutrality check.

declare function setTimeout(handler: () => void, timeoutMs?: number): unknown;
declare function clearTimeout(handle: unknown): void;
declare function setInterval(handler: () => void, timeoutMs?: number): unknown;
declare function clearInterval(handle: unknown): void;

declare var console: {
  log(...args: unknown[]): void;
  warn(...args: unknown[]): void;
  error(...args: unknown[]): void;
  debug(...args: unknown[]): void;
};

declare function atob(data: string): string;
declare function btoa(data: string): string;

declare class TextEncoder {
  encode(input: string): Uint8Array;
}

declare class TextDecoder {
  constructor(label?: string, options?: { fatal?: boolean });
  decode(input?: ArrayBuffer | ArrayBufferView, options?: { stream?: boolean }): string;
}

declare class URLSearchParams {
  constructor(init?: string | Record<string, string>);
  get(name: string): string | null;
  set(name: string, value: string): void;
  append(name: string, value: string): void;
  has(name: string): boolean;
  toString(): string;
  [Symbol.iterator](): IterableIterator<[string, string]>;
}
