import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BucketIconEditor } from "./BucketIconEditor";

const markup = (value: Parameters<typeof BucketIconEditor>[0]["value"]) =>
  renderToStaticMarkup(<BucketIconEditor value={value} letters="PM" onChange={() => {}} />);

describe("BucketIconEditor", () => {
  it("previews the automatic letters and offers them as the placeholder when nothing is set", () => {
    const html = markup({});
    expect(html).toContain('<span class="sb-rail-glyph " aria-hidden="true">PM</span>');
    expect(html).toContain('placeholder="PM"');
    expect(html).toContain(">default<");
    expect(html).toContain(">Upload…<");
  });

  it("previews the chosen color and offers to clear it", () => {
    const html = markup({ color: "#3dd68c" });
    expect(html).toContain('style="--rail-color:#3dd68c"');
    expect(html).toContain('value="#3dd68c"');
    expect(html).toContain(">use default<");
  });

  it("lets a picture take the place of text until it is removed", () => {
    const html = markup({ text: "🐙", image: "data:image/webp;base64,AA==" });
    expect(html).toContain('<img class="sb-rail-image" src="data:image/webp;base64,AA==" alt=""/>');
    const textInput = html.match(/<input[^>]*value="🐙"[^>]*>/)?.[0] ?? "";
    expect(textInput).toContain('disabled=""');
    expect(html).toContain(">Replace…<");
    expect(html).toContain(">remove<");
  });
});
