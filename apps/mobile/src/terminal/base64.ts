// Accessory-row and paste bytes cross the RN/WebView bridge as base64 inside
// JSON. Pure-TS codecs avoid depending on atob/btoa or Buffer, neither of
// which exists in both Hermes and WKWebView.

const ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const REVERSE = new Map<string, number>([...ALPHABET].map((char, index) => [char, index]));

export function bytesToBase64(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const b0 = bytes[i];
    const b1 = bytes[i + 1];
    const b2 = bytes[i + 2];
    out += ALPHABET[b0 >> 2];
    out += ALPHABET[((b0 & 0x03) << 4) | ((b1 ?? 0) >> 4)];
    out += b1 === undefined ? "=" : ALPHABET[((b1 & 0x0f) << 2) | ((b2 ?? 0) >> 6)];
    out += b2 === undefined ? "=" : ALPHABET[b2 & 0x3f];
  }
  return out;
}

export function base64ToBytes(text: string): Uint8Array | null {
  const stripped = text.replace(/=+$/, "");
  if (text.length % 4 !== 0 && text.includes("=")) return null;
  const bits = stripped.length * 6;
  const length = Math.floor(bits / 8);
  if (stripped.length % 4 === 1) return null;
  const bytes = new Uint8Array(length);
  let buffer = 0;
  let bufferBits = 0;
  let offset = 0;
  for (const char of stripped) {
    const value = REVERSE.get(char);
    if (value === undefined) return null;
    buffer = (buffer << 6) | value;
    bufferBits += 6;
    if (bufferBits >= 8) {
      bufferBits -= 8;
      bytes[offset] = (buffer >> bufferBits) & 0xff;
      offset += 1;
    }
  }
  return bytes;
}
