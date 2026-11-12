import { useRef, useState } from "react";
import { RailFace, railColorStyle } from "./BucketRail";
import type { RailIcon } from "./BucketSwitcher";

/** Pixel size of an uploaded picture; twice the rail button so it stays sharp on high-density screens. */
export const ICON_IMAGE_PX = 68;
const ICON_IMAGE_QUALITY = 0.86;
const ICON_TEXT_MAX_CHARS = 4;
const DEFAULT_PICKER_COLOR = "#4da3ff";

/** Center-crops a picture to a square and shrinks it to the rail's size. */
export async function shrinkIconImage(file: File): Promise<string> {
  const bitmap = await createImageBitmap(file);
  const side = Math.min(bitmap.width, bitmap.height);
  const canvas = document.createElement("canvas");
  canvas.width = ICON_IMAGE_PX;
  canvas.height = ICON_IMAGE_PX;
  const context = canvas.getContext("2d");
  if (!context) throw new Error("this browser cannot resize images");
  context.drawImage(
    bitmap,
    (bitmap.width - side) / 2,
    (bitmap.height - side) / 2,
    side,
    side,
    0,
    0,
    ICON_IMAGE_PX,
    ICON_IMAGE_PX,
  );
  bitmap.close();
  const webp = canvas.toDataURL("image/webp", ICON_IMAGE_QUALITY);
  return webp.startsWith("data:image/webp") ? webp : canvas.toDataURL("image/png");
}

export function BucketIconEditor({
  value,
  letters,
  onChange,
}: {
  value: RailIcon;
  /** The automatic letters shown when the bucket has no text or picture of its own. */
  letters: string;
  onChange: (next: RailIcon) => void;
}) {
  const fileRef = useRef<HTMLInputElement>(null);
  const [error, setError] = useState<string | null>(null);

  const upload = async (file: File | undefined) => {
    if (!file) return;
    setError(null);
    try {
      onChange({ ...value, image: await shrinkIconImage(file) });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : "could not read that image");
    }
  };

  return (
    <fieldset className="catalog-fieldset bucket-icon-editor">
      <legend>Rail icon</legend>
      <div className="bucket-icon-row">
        <span
          className={`sb-rail-btn bucket-icon-preview ${value.color ? "has-color" : ""} ${value.image ? "has-image" : ""}`}
          style={railColorStyle(value)}
          aria-hidden="true"
        >
          <RailFace icon={value} letters={letters} />
        </span>
        <div className="bucket-icon-fields">
          <label className="bucket-icon-field">
            <span>Letters or emoji</span>
            <input
              value={value.text ?? ""}
              placeholder={letters}
              maxLength={ICON_TEXT_MAX_CHARS * 2}
              disabled={Boolean(value.image)}
              onChange={(event) => onChange({ ...value, text: event.target.value || undefined })}
            />
          </label>
          <div className="bucket-icon-field">
            <span>Color</span>
            <div className="bucket-icon-inline">
              <input
                type="color"
                aria-label="Rail icon color"
                value={value.color ?? DEFAULT_PICKER_COLOR}
                onChange={(event) => onChange({ ...value, color: event.target.value })}
              />
              {value.color
                ? <button type="button" className="link-btn" onClick={() => onChange({ ...value, color: undefined })}>use default</button>
                : <small>default</small>}
            </div>
          </div>
          <div className="bucket-icon-field">
            <span>Image</span>
            <div className="bucket-icon-inline">
              <button type="button" className="btn" onClick={() => fileRef.current?.click()}>
                {value.image ? "Replace…" : "Upload…"}
              </button>
              {value.image && (
                <button type="button" className="link-btn" onClick={() => onChange({ ...value, image: undefined })}>remove</button>
              )}
              <input
                ref={fileRef}
                type="file"
                accept="image/png,image/jpeg,image/webp,image/gif,image/svg+xml"
                hidden
                onChange={(event) => {
                  void upload(event.target.files?.[0]);
                  event.target.value = "";
                }}
              />
            </div>
          </div>
        </div>
      </div>
      <small>A picture takes the place of letters. Leave everything blank to use {letters}.</small>
      {error && <div className="form-error" role="alert">{error}</div>}
    </fieldset>
  );
}
