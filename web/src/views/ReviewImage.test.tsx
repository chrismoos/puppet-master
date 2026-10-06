import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ReviewImage } from "./ReviewImage";
import type { DiffFile } from "./reviewDiff";
import { ReviewSide } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

describe("ReviewImage", () => {
  const dummyActions = {
    setComposing: () => undefined,
    cancelCompose: () => undefined,
    setDraft: () => undefined,
    submitComment: () => undefined,
  };

  const threadCard = (t: { id: number; original_line: number }) => (
    <div key={t.id} className="test-thread-card">
      Thread {t.id} on line {t.original_line}
    </div>
  );

  it("renders a newly added image with the Added badge and new side URL", () => {
    const file: DiffFile = {
      path: "assets/logo.png",
      rows: [],
      added: 0,
      removed: 0,
      binary: true,
      newFile: true,
      unreadable: null,
    };

    const html = renderToStaticMarkup(
      <ReviewImage
        reviewId={12}
        file={file}
        view=""
        diffSnapshot={null}
        threads={[]}
        composing={null}
        composerKey={null}
        initialDraftBody=""
        actions={dummyActions}
        threadCard={threadCard}
      />,
    );

    expect(html).toContain("Added");
    expect(html).toContain("New image");
    expect(html).toContain("/api/reviews/12/file?file=assets%2Flogo.png&amp;side=new");
    expect(html).toContain("Comment on image");
  });

  it("renders a deleted image with the Deleted badge and old side URL", () => {
    const file: DiffFile = {
      path: "assets/old-logo.png",
      rows: [],
      added: 0,
      removed: 0,
      binary: true,
      deleted: true,
      unreadable: null,
    };

    const html = renderToStaticMarkup(
      <ReviewImage
        reviewId={12}
        file={file}
        view=""
        diffSnapshot={null}
        threads={[]}
        composing={null}
        composerKey={null}
        initialDraftBody=""
        actions={dummyActions}
        threadCard={threadCard}
      />,
    );

    expect(html).toContain("Deleted");
    expect(html).toContain("Deleted image");
    expect(html).toContain("/api/reviews/12/file?file=assets%2Fold-logo.png&amp;side=old");
  });

  it("renders modified image with 2-Up view showing Before and After panes", () => {
    const file: DiffFile = {
      path: "assets/banner.png",
      rows: [],
      added: 0,
      removed: 0,
      binary: true,
      unreadable: null,
    };

    const html = renderToStaticMarkup(
      <ReviewImage
        reviewId={5}
        file={file}
        view="round:2"
        diffSnapshot={100n}
        threads={[]}
        composing={null}
        composerKey={null}
        initialDraftBody=""
        actions={dummyActions}
        threadCard={threadCard}
      />,
    );

    expect(html).toContain("2-Up");
    expect(html).toContain("Swipe");
    expect(html).toContain("Before");
    expect(html).toContain("After");
    expect(html).toContain("/api/reviews/5/file?file=assets%2Fbanner.png&amp;side=old&amp;view=round%3A2");
    expect(html).toContain("/api/reviews/5/file?file=assets%2Fbanner.png&amp;side=new&amp;view=round%3A2");
  });

  it("renders active composer when composing on this image", () => {
    const file: DiffFile = {
      path: "assets/icon.png",
      rows: [],
      added: 0,
      removed: 0,
      binary: true,
      unreadable: null,
    };

    const composing = {
      path: "assets/icon.png",
      line: 1,
      side: ReviewSide.RIGHT,
      excerpt: "assets/icon.png",
      snapshot: null,
    };

    const html = renderToStaticMarkup(
      <ReviewImage
        reviewId={1}
        file={file}
        view=""
        diffSnapshot={null}
        threads={[]}
        composing={composing}
        composerKey="icon"
        initialDraftBody="Make the border rounded."
        actions={dummyActions}
        threadCard={threadCard}
      />,
    );

    expect(html).toContain("review-image-compose");
    expect(html).toContain("Make the border rounded.");
    expect(html).toContain("Save as draft");
    expect(html).toContain("Send");
  });

  it("renders attached threads under the image", () => {
    const file: DiffFile = {
      path: "assets/badge.svg",
      rows: [],
      added: 0,
      removed: 0,
      binary: false,
      unreadable: null,
    };

    const thread = {
      id: 42,
      path: "assets/badge.svg",
      original_line: 1,
      current_line: 1,
      original_side: ReviewSide.RIGHT,
      current_side: ReviewSide.RIGHT,
      original_excerpt: "assets/badge.svg",
      current_excerpt: "assets/badge.svg",
      anchor_status: "same",
      state: "open",
      messages: [],
      created_at: 1000,
      updated_at: 1000,
    };

    const html = renderToStaticMarkup(
      <ReviewImage
        reviewId={1}
        file={file}
        view=""
        diffSnapshot={null}
        threads={[thread]}
        composing={null}
        composerKey={null}
        initialDraftBody=""
        actions={dummyActions}
        threadCard={threadCard}
      />,
    );

    expect(html).toContain("Thread 42 on line 1");
  });
});
