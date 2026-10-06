const TERMINAL_FONT_SIZE_PX = 13;
const TERMINAL_FONT_FAMILY = '"JetBrains Mono Variable"';

/// xterm rasterizes a glyph into its texture atlas the first time it draws it,
/// so a face that arrives after the first paint leaves fallback metrics cached
/// until a colour change forces that glyph to be drawn again. Italic is its own
/// face and needs its own load; weight does not, being an axis inside each file.
export const PRELOADED_FONT_SPECIFIERS = [
  `${TERMINAL_FONT_SIZE_PX}px ${TERMINAL_FONT_FAMILY}`,
  `italic ${TERMINAL_FONT_SIZE_PX}px ${TERMINAL_FONT_FAMILY}`,
];

/// Settles rather than rejects so a face the browser cannot fetch still starts
/// the app, without racing the faces that did load.
export function preloadTerminalFonts(fonts: FontFaceSet = document.fonts): Promise<unknown> {
  return Promise.allSettled(PRELOADED_FONT_SPECIFIERS.map((specifier) => fonts.load(specifier)));
}
