export const ITEM_BODY_MAX_CHARACTERS = 65_536;

export function itemBodyCharacterCount(value: string): number {
  return Array.from(value).length;
}

export function itemBodyLengthError(value: string): string | null {
  const actual = itemBodyCharacterCount(value);
  if (actual <= ITEM_BODY_MAX_CHARACTERS) return null;
  return `Description is ${actual.toLocaleString()} characters; the limit is ${ITEM_BODY_MAX_CHARACTERS.toLocaleString()}.`;
}
