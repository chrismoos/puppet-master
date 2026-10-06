// Hermes has shipped TextEncoder for a while but TextDecoder arrived later,
// and the protobuf runtime needs both. Install pure-TS UTF-8 codecs when the
// globals are missing so codec behavior does not depend on the JS engine.

const REPLACEMENT = 0xfffd;

export function utf8Encode(text: string): Uint8Array {
  const bytes: number[] = [];
  for (let i = 0; i < text.length; i += 1) {
    let code = text.charCodeAt(i);
    if (code >= 0xd800 && code <= 0xdbff && i + 1 < text.length) {
      const low = text.charCodeAt(i + 1);
      if (low >= 0xdc00 && low <= 0xdfff) {
        code = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
        i += 1;
      } else {
        code = REPLACEMENT;
      }
    } else if (code >= 0xd800 && code <= 0xdfff) {
      code = REPLACEMENT;
    }
    if (code < 0x80) {
      bytes.push(code);
    } else if (code < 0x800) {
      bytes.push(0xc0 | (code >> 6), 0x80 | (code & 0x3f));
    } else if (code < 0x10000) {
      bytes.push(0xe0 | (code >> 12), 0x80 | ((code >> 6) & 0x3f), 0x80 | (code & 0x3f));
    } else {
      bytes.push(
        0xf0 | (code >> 18),
        0x80 | ((code >> 12) & 0x3f),
        0x80 | ((code >> 6) & 0x3f),
        0x80 | (code & 0x3f),
      );
    }
  }
  return new Uint8Array(bytes);
}

export function utf8Decode(bytes: Uint8Array): string {
  let out = "";
  let i = 0;
  while (i < bytes.length) {
    const byte = bytes[i];
    let code: number;
    let extra: number;
    if (byte < 0x80) {
      code = byte;
      extra = 0;
    } else if ((byte & 0xe0) === 0xc0) {
      code = byte & 0x1f;
      extra = 1;
    } else if ((byte & 0xf0) === 0xe0) {
      code = byte & 0x0f;
      extra = 2;
    } else if ((byte & 0xf8) === 0xf0) {
      code = byte & 0x07;
      extra = 3;
    } else {
      out += String.fromCharCode(REPLACEMENT);
      i += 1;
      continue;
    }
    let valid = true;
    let consumed = 1;
    for (let k = 1; k <= extra; k += 1) {
      const cont = bytes[i + k];
      if (cont === undefined || (cont & 0xc0) !== 0x80) {
        valid = false;
        break;
      }
      code = (code << 6) | (cont & 0x3f);
      consumed += 1;
    }
    if (!valid) {
      // Consume the maximal subpart (lead plus valid continuations) so a
      // truncated sequence becomes a single replacement character.
      out += String.fromCharCode(REPLACEMENT);
      i += consumed;
      continue;
    }
    i += extra + 1;
    const overlong =
      (extra === 1 && code < 0x80) || (extra === 2 && code < 0x800) || (extra === 3 && code < 0x10000);
    if (overlong || code > 0x10ffff || (code >= 0xd800 && code <= 0xdfff)) {
      out += String.fromCharCode(REPLACEMENT);
      continue;
    }
    if (code < 0x10000) {
      out += String.fromCharCode(code);
    } else {
      const offset = code - 0x10000;
      out += String.fromCharCode(0xd800 + (offset >> 10), 0xdc00 + (offset & 0x3ff));
    }
  }
  return out;
}

class PolyfillTextEncoder {
  readonly encoding = "utf-8";
  encode(input = ""): Uint8Array {
    return utf8Encode(input);
  }
}

class PolyfillTextDecoder {
  readonly encoding = "utf-8";
  decode(input?: ArrayBuffer | ArrayBufferView): string {
    if (!input) return "";
    const bytes =
      input instanceof Uint8Array
        ? input
        : ArrayBuffer.isView(input)
          ? new Uint8Array(input.buffer, input.byteOffset, input.byteLength)
          : new Uint8Array(input);
    return utf8Decode(bytes);
  }
}

export function installTextCodecPolyfill(scope: Record<string, unknown> = globalThis as unknown as Record<string, unknown>): void {
  if (typeof scope["TextEncoder"] !== "function") scope["TextEncoder"] = PolyfillTextEncoder;
  if (typeof scope["TextDecoder"] !== "function") scope["TextDecoder"] = PolyfillTextDecoder;
}
