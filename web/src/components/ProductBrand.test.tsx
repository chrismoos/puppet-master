import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ProductBrand } from "./ProductBrand";

describe("ProductBrand", () => {
  it("uses the explicit lockup by default and keeps the product name accessible", () => {
    const markup = renderToStaticMarkup(<ProductBrand />);

    expect(markup).toContain('class="product-brand product-brand-lockup"');
    expect(markup).toContain('src="/brand/puppet-master-lockup-color-dark.svg"');
    expect(markup).toContain('src="/brand/puppet-master-lockup-color-light.svg"');
    expect(markup).toContain('alt="Puppet Master"');
  });

  it("makes the compact navigation mark decorative inside its labelled control", () => {
    const markup = renderToStaticMarkup(<ProductBrand variant="mark" decorative />);

    expect(markup).toContain('class="product-brand product-brand-mark"');
    expect(markup).toContain('src="/brand/puppet-master-mark-color-dark.svg"');
    expect(markup).toContain('src="/brand/puppet-master-mark-color-light.svg"');
    expect(markup).toContain('alt=""');
    expect(markup).toContain('aria-hidden="true"');
  });

  it("ships one variant per background and marks which is which", () => {
    const markup = renderToStaticMarkup(<ProductBrand variant="mark" />);

    expect(markup).toContain('class="product-brand-image for-dark-bg"');
    expect(markup).toContain('class="product-brand-image for-light-bg"');
    expect(markup).not.toContain("filter");
  });
});
