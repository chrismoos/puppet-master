import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BucketRail, bucketMonogram, RailFace, railMonograms } from "./BucketRail";
import type { SwitcherBucket } from "./BucketSwitcher";
import type { SidebarScope } from "./sidebarScope";

const named = (...names: string[]) => names.map((name, index) => ({ id: String(index + 1), name }));

describe("bucket monograms", () => {
  it("takes the initials of two words or the first two letters of one", () => {
    expect(bucketMonogram("puppet-master")).toBe("PM");
    expect(bucketMonogram("svelte_rust")).toBe("SR");
    expect(bucketMonogram("my big project")).toBe("MB");
    expect(bucketMonogram("bladerun")).toBe("BL");
    expect(bucketMonogram("x")).toBe("X");
    expect(bucketMonogram("")).toBe("?");
  });

  it("keeps the plain monogram for every bucket whose letters are already unique", () => {
    expect([...railMonograms(named("puppet-master", "bladerun", "infra")).values()]).toEqual(["PM", "BL", "IN"]);
  });

  it("gives colliding buckets distinct letters in bucket order", () => {
    const letters = railMonograms(named("puppet-master", "pm-ios", "pm-mobile"));
    expect(new Set(letters.values()).size).toBe(3);
    expect(letters.get("1")).toBe("PM");
    expect(letters.get("2")).toBe("PI");
    expect(letters.get("3")).toBe("PO");
  });

  it("never hands out letters a unique bucket already shows", () => {
    const letters = railMonograms(named("pi", "pm-ios", "pm-infra"));
    expect(letters.get("1")).toBe("PI");
    expect(new Set(letters.values()).size).toBe(3);
  });
});

describe("RailFace", () => {
  it("prefers a picture, then the bucket's own text, then its letters", () => {
    expect(renderToStaticMarkup(<RailFace icon={{ image: "data:image/png;base64,AA==", text: "X" }} letters="PM" />))
      .toBe('<img class="sb-rail-image" src="data:image/png;base64,AA==" alt=""/>');
    expect(renderToStaticMarkup(<RailFace icon={{ text: "🐙" }} letters="PM" />)).toContain(">🐙<");
    expect(renderToStaticMarkup(<RailFace letters="PM" />)).toContain(">PM<");
  });

  it("shrinks text of three or more characters", () => {
    expect(renderToStaticMarkup(<RailFace icon={{ text: "PUP" }} letters="PM" />)).toContain("is-long");
    expect(renderToStaticMarkup(<RailFace letters="PM" />)).not.toContain("is-long");
  });
});

describe("BucketRail", () => {
  const buckets: SwitcherBucket[] = [
    { id: "1", name: "puppet-master", live: 12, needsInput: 2 },
    { id: "2", name: "infra", live: 4, needsInput: 0, icon: { color: "#3dd68c" } },
  ];
  const markup = (scope: SidebarScope, searchOpen = false) => renderToStaticMarkup(
    <BucketRail scope={scope} buckets={buckets} searchOpen={searchOpen} onPickScope={() => {}} onOpenSearch={() => {}} />,
  );

  it("offers every bucket, the needs-you total, search, then each bucket with its badge", () => {
    const html = markup({ kind: "all" });
    const labels = [...html.matchAll(/aria-label="([^"]+)"/g)].map((match) => match[1]);
    expect(labels).toEqual(["buckets", "All buckets", "Needs you, 2 need input", "Search sessions", "puppet-master, 2 need input", "infra"]);
  });

  it("presses only the scoped button and tints a bucket with its own color", () => {
    const html = markup({ kind: "bucket", bucketId: "2" });
    expect(html.match(/aria-pressed="true"/g)).toHaveLength(1);
    expect(html).toMatch(/class="sb-rail-btn is-current has-color\s*" style="--rail-color:#3dd68c" aria-pressed="true"/);
  });

  it("says whether search is open on the search button", () => {
    expect(markup({ kind: "all" }, true)).toMatch(/aria-label="Search sessions" aria-expanded="true"/);
    expect(markup({ kind: "all" }, false)).toMatch(/aria-label="Search sessions" aria-expanded="false"/);
  });
});
