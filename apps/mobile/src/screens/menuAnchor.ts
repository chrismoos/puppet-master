/** Where the menu opens on screen: below the button that opened it, lined up
 * with its right edge, in window coordinates. */
export interface MenuAnchor {
  top: number;
  right: number;
}

/** Space between the button and the menu below it. */
const ANCHOR_GAP = 6;

export function anchorBelow(
  frame: { x: number; y: number; width: number; height: number },
  windowWidth: number,
): MenuAnchor {
  return {
    top: frame.y + frame.height + ANCHOR_GAP,
    right: Math.max(0, windowWidth - (frame.x + frame.width)),
  };
}
