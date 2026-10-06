import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FullscreenReview } from "./FullscreenReview";
import { ReviewPage } from "./ReviewPage";

vi.mock("./ReviewPage", () => ({ ReviewPage: vi.fn(() => null) }));
vi.mock("../router", () => ({ navigate: vi.fn() }));

const renderedReview = vi.mocked(ReviewPage);

afterEach(() => {
  vi.clearAllMocks();
  vi.unstubAllGlobals();
});

describe("fullscreen review", () => {
  it("renders the review alone and preserves its reading location", () => {
    const html = renderToStaticMarkup(<FullscreenReview
      route={{ name: "review", id: 4, view: "changes", file: "a.txt", thread: 2 }}
      ownsWindow={false}
    />);
    expect(html).toContain("shell-fullbleed is-focus-mode review-window");
    expect(html).not.toContain("sidebar");
    expect(html).not.toContain("terminal");
    expect(renderedReview.mock.calls[0][0]).toMatchObject({
      id: 4,
      at: { view: "changes", file: "a.txt", thread: 2 },
    });
  });

  it("consumes finish navigation while closing a review's own window", () => {
    const close = vi.fn();
    vi.stubGlobal("window", { close });
    renderToStaticMarkup(<FullscreenReview route={{ name: "review", id: 4 }} ownsWindow />);
    expect(renderedReview.mock.calls[0][0].onFinished?.()).toBe(true);
    expect(close).toHaveBeenCalledOnce();
  });

  it("allows finish navigation in the reader's existing window", () => {
    const close = vi.fn();
    vi.stubGlobal("window", { close });
    renderToStaticMarkup(<FullscreenReview route={{ name: "review", id: 4 }} ownsWindow={false} />);
    expect(renderedReview.mock.calls[0][0].onFinished?.()).toBe(false);
    expect(close).not.toHaveBeenCalled();
  });
});
