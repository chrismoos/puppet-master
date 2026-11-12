import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BucketSwitcher, matchingBuckets, needsElsewhere, type SwitcherBucket } from "./BucketSwitcher";
import type { SidebarScope } from "./sidebarScope";

const BUCKETS: SwitcherBucket[] = [
  { id: "1", name: "puppet-master", live: 18, needsInput: 2 },
  { id: "2", name: "bladerun", live: 9, needsInput: 1 },
  { id: "3", name: "infra", live: 4, needsInput: 0 },
];

function markup(props: Partial<Parameters<typeof BucketSwitcher>[0]> = {}): string {
  return renderToStaticMarkup(
    <BucketSwitcher
      scope={{ kind: "bucket", bucketId: "1" }}
      buckets={BUCKETS}
      open={false}
      onOpenChange={() => {}}
      query=""
      onQuery={() => {}}
      sessions={[]}
      resultTotal={null}
      loading={false}
      onPickScope={() => {}}
      onPickSession={() => {}}
      onShowAllResults={() => {}}
      {...props}
    />,
  );
}

const text = (html: string) => html.replace(/<[^>]+>/g, " ").replace(/\s+/g, " ").trim();

describe("the closed bucket switcher", () => {
  it("names the scoped bucket and counts what needs input in the others", () => {
    const html = markup();
    expect(html).toContain('<span class="sb-switcher-current">puppet-master</span>');
    expect(html).toContain('title="1 session in other buckets need input">+1</span>');
    expect(html).not.toContain("sb-switcher-menu");
  });

  it("shows no elsewhere badge for every bucket or the needs-you list", () => {
    for (const scope of [{ kind: "all" }, { kind: "needs" }] as SidebarScope[]) {
      expect(markup({ scope })).not.toContain("sb-switcher-elsewhere");
    }
    expect(text(markup({ scope: { kind: "all" } }))).toContain("All buckets");
    expect(text(markup({ scope: { kind: "needs" } }))).toContain("Needs you");
  });

  it("counts nothing elsewhere when only the scoped bucket needs input", () => {
    expect(needsElsewhere({ kind: "bucket", bucketId: "1" }, [BUCKETS[0], BUCKETS[2]])).toBe(0);
    expect(needsElsewhere({ kind: "bucket", bucketId: "3" }, BUCKETS)).toBe(3);
  });
});

describe("the open bucket switcher", () => {
  it("offers every bucket, the needs-you list and each bucket with its counts", () => {
    const html = markup({ open: true });
    expect(text(html)).toBe(
      "puppet-master +1 ▾ All buckets 31 Needs you 3 puppet-master 2 18 bladerun 1 9 infra 4",
    );
    expect(html).toMatch(/class="sb-switcher-item has-columns is-active"[^>]*>.*?All buckets/);
    expect(html).toMatch(/class="sb-switcher-item has-columns is-current"[^>]*><span class="sb-switcher-name">puppet-master/);
  });

  it("narrows to matching buckets and lists matching sessions with a way to see them all", () => {
    const html = markup({
      open: true,
      query: "in",
      sessions: [
        { id: "5", name: "Lima swap", bucketName: "infra" },
        { id: "6", name: "Unfiled thing", bucketName: null },
      ],
      resultTotal: 14,
    });
    expect(text(html)).toBe(
      "puppet-master +1 ▾ Buckets infra 4 Sessions Lima swap infra Unfiled thing Show all 14 results",
    );
  });

  it("caps the listed sessions and says when nothing matches", () => {
    const sessions = Array.from({ length: 12 }, (_, i) => ({ id: String(i), name: `s${i}`, bucketName: null }));
    const capped = markup({ open: true, query: "zzz", sessions, resultTotal: 12 });
    expect(capped).toContain(">s7<");
    expect(capped).not.toContain(">s8<");
    expect(text(markup({ open: true, query: "zzz", resultTotal: 0 }))).toContain("no matches");
    expect(text(markup({ open: true, query: "zzz", loading: true }))).toContain("searching…");
  });

  it("matches bucket names without regard to case or surrounding space", () => {
    expect(matchingBuckets(BUCKETS, "  BLADE ").map((b) => b.id)).toEqual(["2"]);
    expect(matchingBuckets(BUCKETS, "").map((b) => b.id)).toEqual(["1", "2", "3"]);
  });
});
