# Puppet Master product identity

These are the canonical web assets for the Articulated Operator identity.

- The lockup is the explicit authentication identity.
- The mark is the compact navigation identity.
- The tiled app icon supplies favicon, touch-icon, and install metadata.
- The mobile app icon (`apps/mobile/assets/icon.svg`) is the same artwork on a square tile, because iOS masks the corners itself; keep the two in step when the mark changes.

The suffix identifies the intended background. Both variants ship because the application palette has a light and a dark appearance, and `ProductBrand` renders both and lets CSS show the one the current appearance calls for. Each variant is drawn for its background: never recolour one with a CSS filter to stand in for the other.
