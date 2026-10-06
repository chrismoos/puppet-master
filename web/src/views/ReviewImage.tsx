import { useState } from "react";
import type { ReactNode } from "react";
import type { DiffFile } from "./reviewDiff";
import { ReviewSide } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ReviewComposer } from "./ReviewPage";

export type ImageDiffMode = "2-up" | "swipe" | "after" | "before";

export interface ReviewImageProps<T = any> {
  reviewId: number | string;
  file: DiffFile;
  view: string;
  diffSnapshot: bigint | null;
  threads: readonly T[];
  composing: {
    path: string;
    line: number;
    side: ReviewSide;
    excerpt: string;
    snapshot: bigint | null;
  } | null;
  composerKey: string | null;
  initialDraftBody: string;
  actions: {
    setComposing: (c: {
      path: string;
      line: number;
      side: ReviewSide;
      excerpt: string;
      snapshot: bigint | null;
    }) => void;
    cancelCompose: () => void;
    setDraft: (key: string | null, body: string) => void;
    submitComment: (bodyOrSend: string | boolean, send?: boolean) => void;
  };
  threadCard: (t: T) => ReactNode;
}

interface ImageDim {
  w: number;
  h: number;
}

export function ReviewImage({
  reviewId,
  file,
  view,
  diffSnapshot,
  threads,
  composing,
  composerKey,
  initialDraftBody,
  actions,
  threadCard,
}: ReviewImageProps) {
  const [mode, setMode] = useState<ImageDiffMode>("2-up");
  const [swipePct, setSwipePct] = useState(50);
  const [oldDim, setOldDim] = useState<ImageDim | null>(null);
  const [newDim, setNewDim] = useState<ImageDim | null>(null);
  const [oldError, setOldError] = useState(false);
  const [newError, setNewError] = useState(false);

  const isDeleted = Boolean(file.deleted);
  const isAdded = Boolean(file.newFile) || (!isDeleted && oldError);
  const hasBoth = !isDeleted && !isAdded && !oldError && !newError;

  const basePath = `/api/reviews/${reviewId}/file?file=${encodeURIComponent(file.path)}`;
  const viewQuery = view ? `&view=${encodeURIComponent(view)}` : "";
  const oldSrc = `${basePath}&side=old${viewQuery}`;
  const newSrc = `${basePath}&side=new${viewQuery}`;

  const composingHere = composing !== null && composing.path === file.path;

  const onAddComment = () => {
    actions.setComposing({
      path: file.path,
      line: 1,
      side: ReviewSide.RIGHT,
      excerpt: file.path,
      snapshot: diffSnapshot,
    });
  };

  const formatDim = (dim: ImageDim | null) => (dim ? `${dim.w} × ${dim.h} px` : "loading…");

  return (
    <div className="review-image-view">
      <div className="review-image-toolbar">
        <div className="review-image-modes">
          {hasBoth && (
            <div className="review-image-segmented" role="tablist">
              <button
                type="button"
                className={`review-image-mode-btn ${mode === "2-up" ? "is-active" : ""}`}
                onClick={() => setMode("2-up")}
              >
                2-Up
              </button>
              <button
                type="button"
                className={`review-image-mode-btn ${mode === "swipe" ? "is-active" : ""}`}
                onClick={() => setMode("swipe")}
              >
                Swipe
              </button>
              <button
                type="button"
                className={`review-image-mode-btn ${mode === "after" ? "is-active" : ""}`}
                onClick={() => setMode("after")}
              >
                After
              </button>
              <button
                type="button"
                className={`review-image-mode-btn ${mode === "before" ? "is-active" : ""}`}
                onClick={() => setMode("before")}
              >
                Before
              </button>
            </div>
          )}
          {isAdded && <span className="review-image-badge is-added">Added</span>}
          {isDeleted && <span className="review-image-badge is-deleted">Deleted</span>}
          {!hasBoth && !isAdded && !isDeleted && (
            <span className="review-image-badge">Modified</span>
          )}
        </div>

        <div className="review-image-meta">
          {hasBoth && oldDim && newDim && (
            <span className="review-image-dimensions">
              {oldDim.w === newDim.w && oldDim.h === newDim.h ? (
                `${newDim.w} × ${newDim.h} px`
              ) : (
                <>
                  <span className="dim-old">{oldDim.w} × {oldDim.h}</span>
                  {" → "}
                  <span className="dim-new">{newDim.w} × {newDim.h} px</span>
                </>
              )}
            </span>
          )}
          {isAdded && newDim && (
            <span className="review-image-dimensions">{formatDim(newDim)}</span>
          )}
          {isDeleted && oldDim && (
            <span className="review-image-dimensions">{formatDim(oldDim)}</span>
          )}

          {!composingHere && (
            <button
              type="button"
              className="btn btn-sm review-image-comment-btn"
              onClick={onAddComment}
            >
              Comment on image
            </button>
          )}
        </div>
      </div>

      <div className="review-image-display">
        {isDeleted ? (
          <div className="review-image-pane">
            <span className="review-image-pane-title">Deleted image</span>
            <div className="review-image-checkerboard">
              <img
                src={oldSrc}
                alt={file.path}
                onLoad={(e) =>
                  setOldDim({
                    w: e.currentTarget.naturalWidth,
                    h: e.currentTarget.naturalHeight,
                  })
                }
                onError={() => setOldError(true)}
              />
            </div>
          </div>
        ) : isAdded ? (
          <div className="review-image-pane">
            <span className="review-image-pane-title">New image</span>
            <div className="review-image-checkerboard">
              <img
                src={newSrc}
                alt={file.path}
                onLoad={(e) =>
                  setNewDim({
                    w: e.currentTarget.naturalWidth,
                    h: e.currentTarget.naturalHeight,
                  })
                }
                onError={() => setNewError(true)}
              />
            </div>
          </div>
        ) : mode === "2-up" ? (
          <div className="review-image-2up">
            <div className="review-image-pane">
              <div className="review-image-pane-head">
                <span className="review-image-pane-title">Before</span>
                <span className="review-image-pane-dim">{formatDim(oldDim)}</span>
              </div>
              <div className="review-image-checkerboard">
                <img
                  src={oldSrc}
                  alt={`Before: ${file.path}`}
                  onLoad={(e) =>
                    setOldDim({
                      w: e.currentTarget.naturalWidth,
                      h: e.currentTarget.naturalHeight,
                    })
                  }
                  onError={() => setOldError(true)}
                />
              </div>
            </div>
            <div className="review-image-pane">
              <div className="review-image-pane-head">
                <span className="review-image-pane-title">After</span>
                <span className="review-image-pane-dim">{formatDim(newDim)}</span>
              </div>
              <div className="review-image-checkerboard">
                <img
                  src={newSrc}
                  alt={`After: ${file.path}`}
                  onLoad={(e) =>
                    setNewDim({
                      w: e.currentTarget.naturalWidth,
                      h: e.currentTarget.naturalHeight,
                    })
                  }
                  onError={() => setNewError(true)}
                />
              </div>
            </div>
          </div>
        ) : mode === "swipe" ? (
          <div className="review-image-swipe-wrapper">
            <div className="review-image-swipe-container review-image-checkerboard">
              <img
                className="review-image-swipe-under"
                src={oldSrc}
                alt={`Before: ${file.path}`}
                onLoad={(e) =>
                  setOldDim({
                    w: e.currentTarget.naturalWidth,
                    h: e.currentTarget.naturalHeight,
                  })
                }
                onError={() => setOldError(true)}
              />
              <div
                className="review-image-swipe-over"
                style={{ clipPath: `inset(0 ${100 - swipePct}% 0 0)` }}
              >
                <img
                  src={newSrc}
                  alt={`After: ${file.path}`}
                  onLoad={(e) =>
                    setNewDim({
                      w: e.currentTarget.naturalWidth,
                      h: e.currentTarget.naturalHeight,
                    })
                  }
                  onError={() => setNewError(true)}
                />
              </div>
              <div
                className="review-image-swipe-divider"
                style={{ left: `${swipePct}%` }}
              />
            </div>
            <div className="review-image-swipe-controls">
              <span>Before</span>
              <input
                type="range"
                min="0"
                max="100"
                value={swipePct}
                onChange={(e) => setSwipePct(Number(e.target.value))}
                className="review-image-slider"
                aria-label="Image comparison swipe position"
              />
              <span>After</span>
            </div>
          </div>
        ) : mode === "before" ? (
          <div className="review-image-pane">
            <span className="review-image-pane-title">Before ({formatDim(oldDim)})</span>
            <div className="review-image-checkerboard">
              <img
                src={oldSrc}
                alt={`Before: ${file.path}`}
                onLoad={(e) =>
                  setOldDim({
                    w: e.currentTarget.naturalWidth,
                    h: e.currentTarget.naturalHeight,
                  })
                }
                onError={() => setOldError(true)}
              />
            </div>
          </div>
        ) : (
          <div className="review-image-pane">
            <span className="review-image-pane-title">After ({formatDim(newDim)})</span>
            <div className="review-image-checkerboard">
              <img
                src={newSrc}
                alt={`After: ${file.path}`}
                onLoad={(e) =>
                  setNewDim({
                    w: e.currentTarget.naturalWidth,
                    h: e.currentTarget.naturalHeight,
                  })
                }
                onError={() => setNewError(true)}
              />
            </div>
          </div>
        )}
      </div>

      {composingHere && (
        <ReviewComposer
          className="review-image-compose"
          initialBody={initialDraftBody}
          placeholder="Comment on this image…"
          saveDraft={(body) => actions.setDraft(composerKey, body)}
          onSubmit={(body, send) => actions.submitComment(body, send)}
          onCancel={actions.cancelCompose}
        />
      )}

      {threads.length > 0 && (
        <div className="review-image-threads">
          {threads.map((t) => threadCard(t))}
        </div>
      )}
    </div>
  );
}
