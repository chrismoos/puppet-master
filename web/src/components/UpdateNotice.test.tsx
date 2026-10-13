import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { BuildInfo } from "../api/auth";
import { BuildStamp, UpdateDialog, buildStampTitle, checkedAgo, releaseStateNote } from "./UpdateNotice";

const HOUR = 3_600_000;

function build(overrides: Partial<BuildInfo> = {}): BuildInfo {
  return {
    version: "0.10.3-dev.2",
    gitRev: "e574dde",
    channel: "stable",
    latestVersion: null,
    updateAvailable: null,
    latestCheckedAtUnixMs: null,
    hostPlanePort: null,
    publicUrl: null,
    installCommand: "",
    platform: "linux",
    forwardMount: null,
    ...overrides,
  };
}

const behind = build({ latestVersion: "0.10.4", updateAvailable: "0.10.4" });

describe("the build stamp", () => {
  /// Nothing to click means nothing that looks clickable: a controller on
  /// the newest build is the state the stamp is in almost all the time.
  it("is plain text while no newer build is published", () => {
    const html = renderToStaticMarkup(<BuildStamp build={build()} />);
    expect(html).toContain("v0.10.3-dev.2");
    expect(html).not.toContain("<button");
    expect(html).not.toContain("update-glyph");
  });

  it("becomes a dialog button carrying the glyph once one is", () => {
    const html = renderToStaticMarkup(<BuildStamp build={behind} />);
    expect(html).toContain("<button");
    expect(html).toContain('aria-haspopup="dialog"');
    expect(html).toContain("update-glyph");
  });

  /// The topbar shows the version alone, so the rev has to stay reachable
  /// somewhere: it is what tells two builds of one version apart.
  it("keeps the rev in the tooltip rather than the label", () => {
    const html = renderToStaticMarkup(<BuildStamp build={behind} />);
    expect(html).toContain("running 0.10.3-dev.2 · e574dde");
    expect(html).not.toContain(">v0.10.3-dev.2 · e574dde");
  });

  it("renders nothing before the daemon has answered", () => {
    expect(renderToStaticMarkup(<BuildStamp build={null} />)).toBe("");
  });
});

describe("the stamp tooltip", () => {
  it("names the published build and its channel when behind", () => {
    expect(buildStampTitle(build({ updateAvailable: "0.10.4", channel: "dev" }))).toBe(
      "running 0.10.3-dev.2 · e574dde — 0.10.4 is published on dev",
    );
  });

  it("says only what is running when there is nothing newer", () => {
    expect(buildStampTitle(build())).toBe("running 0.10.3-dev.2 · e574dde");
  });
});

describe("the check time", () => {
  /// The poll runs every six hours, so the age of the answer is part of
  /// the answer. A daemon that has never read the manifest says nothing
  /// rather than implying it looked just now.
  it("reads as how long ago the daemon looked", () => {
    const now = 1_700_000_000_000;
    expect(checkedAgo(build({ latestCheckedAtUnixMs: now - 2 * HOUR }), now)).toBe("2 h ago");
  });

  it("is empty when no check has landed", () => {
    expect(checkedAgo(build(), 1_700_000_000_000)).toBe("");
  });
});

describe("the update dialog", () => {
  it("shows the command and the restart that follows it", () => {
    const html = renderToStaticMarkup(<UpdateDialog build={behind} onClose={() => {}} />);
    expect(html).toContain("Update available");
    expect(html).toContain("Run the following on the controller to get the latest version:");
    expect(html).toContain("pm update");
    expect(html).toContain("After the update is fetched, restart your daemon.");
  });

  /// It explains an update rather than applying one: a button that
  /// installed would be a button that restarts the daemon holding the
  /// reader's sessions.
  it("offers no action that changes the daemon", () => {
    const html = renderToStaticMarkup(<UpdateDialog build={behind} onClose={() => {}} />);
    expect(html).toContain(">Close</button>");
    expect(html).not.toMatch(/Update and restart|Install now/);
  });

  it("names both versions and the channel they are compared on", () => {
    const html = renderToStaticMarkup(
      <UpdateDialog build={build({ updateAvailable: "0.11.0", channel: "dev" })} onClose={() => {}} />,
    );
    expect(html).toContain("0.10.3-dev.2");
    expect(html).toContain("0.11.0");
    expect(html).toContain("dev");
  });
});

describe("what About says when nothing newer is published", () => {
  it("calls the controller current once it has read a release", () => {
    expect(releaseStateNote(build({ latestVersion: "0.10.3-dev.2" }))).toContain("newest published build");
  });

  /// A daemon that has never reached the manifest — offline, just
  /// started, or a build that cannot verify releases — knows nothing
  /// about what is published, which must not read as being current.
  it("does not claim to be current before any check has landed", () => {
    const note = releaseStateNote(build());
    expect(note).toContain("has not read its release channel yet");
    expect(note).not.toContain("newest published build");
  });
});
