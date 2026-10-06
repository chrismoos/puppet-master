/**
 * Decodes the payload of an OSC 52 clipboard sequence — the mechanism
 * TUIs like Claude Code and Codex use to copy to the host clipboard.
 * The data is `<selection>;<base64>`; returns the decoded UTF-8 text for
 * a write, or null for a malformed sequence or a read request ("?"),
 * which is ignored so an application cannot read the user's clipboard.
 */
export function decodeOsc52(data: string): string | null {
  const sep = data.indexOf(";");
  if (sep === -1) return null;
  const payload = data.slice(sep + 1);
  if (!payload || payload === "?") return null;
  try {
    const binary = atob(payload);
    const bytes = Uint8Array.from(binary, (c) => c.charCodeAt(0));
    return new TextDecoder().decode(bytes);
  } catch {
    return null;
  }
}
