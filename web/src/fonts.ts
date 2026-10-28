const TERMINAL_FONT_SIZE_PX = 13;
const TERMINAL_FONT_FAMILY = '"JetBrains Mono Variable"';

/// Cell measurements need the bundled face before the first terminal paint.
/// Italic is loaded separately; weight is an axis in the same font files.
export const PRELOADED_FONT_SPECIFIERS = [
  `${TERMINAL_FONT_SIZE_PX}px ${TERMINAL_FONT_FAMILY}`,
  `italic ${TERMINAL_FONT_SIZE_PX}px ${TERMINAL_FONT_FAMILY}`,
];

/// Settles rather than rejects so a face the browser cannot fetch still starts
/// the app, without racing the faces that did load.
export function preloadTerminalFonts(fonts: FontFaceSet = document.fonts): Promise<unknown> {
  return Promise.allSettled(PRELOADED_FONT_SPECIFIERS.map((specifier) => fonts.load(specifier)));
}
