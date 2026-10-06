const BRAND_ASSETS = {
  lockup: {
    dark: "/brand/puppet-master-lockup-color-dark.svg",
    light: "/brand/puppet-master-lockup-color-light.svg",
  },
  mark: {
    dark: "/brand/puppet-master-mark-color-dark.svg",
    light: "/brand/puppet-master-mark-color-light.svg",
  },
} as const;

export type ProductBrandVariant = keyof typeof BRAND_ASSETS;

/// Both background variants are rendered and CSS shows the one the page
/// appearance calls for. The identity is drawn for its background rather
/// than filtered into place, so neither version is recoloured.
export function ProductBrand({
  variant = "lockup",
  decorative = false,
}: {
  variant?: ProductBrandVariant;
  decorative?: boolean;
}) {
  const assets = BRAND_ASSETS[variant];
  return (
    <span className={`product-brand product-brand-${variant}`}>
      <img
        className="product-brand-image for-dark-bg"
        src={assets.dark}
        alt={decorative ? "" : "Puppet Master"}
        aria-hidden={decorative || undefined}
      />
      <img
        className="product-brand-image for-light-bg"
        src={assets.light}
        alt={decorative ? "" : "Puppet Master"}
        aria-hidden={decorative || undefined}
      />
    </span>
  );
}
