/**
 * Pure arithmetic: how many points does the keyboard overlap the view?
 * Used by KeyboardAvoidingRoot and testable without React Native.
 */
export function computeKeyboardOverlap(
  viewBottomY: number,
  keyboardTopY: number,
): number {
  if (keyboardTopY >= viewBottomY) return 0;
  return viewBottomY - keyboardTopY;
}
