import { describe, expect, it } from "vitest";
import { isOpenableUrl, isPlaintextController } from "./links";

describe("isOpenableUrl", () => {
  it("opens the web", () => {
    for (const url of [
      "https://example.com",
      "http://example.com/path?q=1",
      "HTTPS://EXAMPLE.COM",
      "  https://example.com  ",
    ]) {
      expect(isOpenableUrl(url), url).toBe(true);
    }
  });

  /// A link arrives from agent output, and iOS hands a URL to whichever app
  /// claims its scheme, so every scheme but the web is a way for that output to
  /// reach somewhere this app did not mean to send it.
  it("refuses every other scheme", () => {
    for (const url of [
      "file:///etc/passwd",
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "puppet-master://enrol?token=stolen",
      "tel:+15555555555",
      "sms:+15555555555",
      "mailto:someone@example.com",
      "itms-apps://apps.apple.com/app/id1",
      "shortcuts://run-shortcut?name=wipe",
      "ftp://example.com",
      "//example.com",
      "example.com",
      "",
    ]) {
      expect(isOpenableUrl(url), url).toBe(false);
    }
  });

  /// The check is anchored, so a web URL buried inside another scheme does not
  /// carry the whole string past it.
  it("is not satisfied by http appearing later in the string", () => {
    expect(isOpenableUrl("shortcuts://run?url=https://example.com")).toBe(false);
    expect(isOpenableUrl("x-safe://https://example.com")).toBe(false);
  });
});

describe("isPlaintextController", () => {
  /// ATS is off app-wide and cannot be narrowed without breaking the terminal,
  /// so the app's remaining job is to say when the transport is plaintext
  /// rather than leave it looking the same as a protected one.
  it("names a plain http controller", () => {
    for (const url of ["http://10.0.0.5:7676", "HTTP://pm.example", "  http://pm.example  "]) {
      expect(isPlaintextController(url), url).toBe(true);
    }
  });

  it("stays quiet for https", () => {
    for (const url of ["https://pm.example", "HTTPS://pm.example"]) {
      expect(isPlaintextController(url), url).toBe(false);
    }
  });

  /// Not satisfied by http appearing anywhere later, which would warn about a
  /// perfectly good https controller.
  it("is anchored", () => {
    expect(isPlaintextController("https://pm.example/?next=http://x")).toBe(false);
    expect(isPlaintextController("")).toBe(false);
  });
});
