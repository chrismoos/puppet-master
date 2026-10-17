/**
 * The words to tell the recognizer to expect.
 *
 * Recognition is trained on English, so a name it has never heard comes
 * out as the nearest ordinary word: "iDEN" is heard as "den". The words
 * the user is about to say are the ones already on their dashboard, so
 * the list is built from what their sessions are called rather than
 * from anything they have to maintain.
 *
 * Expect little of it where recognition runs on the device, which is
 * where this app runs it: the contextual-strings list it feeds appears
 * to be ignored there, and biasing an on-device model takes a custom
 * language model built from the same words.
 */

/// How many phrases to hand the recognizer. A long list dilutes the
/// weight each phrase carries, so this keeps the most useful ones.
const MOST_PHRASES = 60;

/// Longer than this is a sentence, not a name, and the user does not say
/// a sentence back verbatim. Its unusual words still count.
const MOST_WORDS_IN_A_PHRASE = 6;

/// Shorter than this is either a common word or initials the recognizer
/// spells out anyway.
const SHORTEST_WORD = 3;

/** What a session has to carry to contribute to the vocabulary. */
export type NamedWork = {
  goal?: string;
  name?: string;
  headline?: string;
};

/**
 * The phrases worth biasing recognition toward, unusual words first
 * because they are what gets misheard and what the user says on its
 * own.
 */
export function spokenVocabulary(work: Iterable<NamedWork>): string[] {
  const words: string[] = [];
  const phrases: string[] = [];
  for (const one of work) {
    for (const line of [one.goal, one.name, one.headline]) {
      const phrase = (line ?? "").trim();
      if (!phrase) continue;
      const parts = phrase.split(/[^\p{L}\p{N}]+/u).filter(Boolean);
      if (parts.length <= MOST_WORDS_IN_A_PHRASE) phrases.push(phrase);
      for (const word of parts) {
        if (isUnusual(word)) words.push(word);
      }
    }
  }
  return distinct([...words, ...phrases]).slice(0, MOST_PHRASES);
}

/**
 * Whether a word is one recognition is likely to get wrong: a product,
 * a codename or a model number rather than English. A capital anywhere
 * but the front is not how an English word is written.
 */
function isUnusual(word: string): boolean {
  if (word.length < SHORTEST_WORD) return false;
  if (/\p{N}/u.test(word)) return true;
  return /\p{Lu}/u.test(word.slice(1)) || word === word.toUpperCase();
}

/// Keeps the first spelling of each, because the spelling is what the
/// recognizer is being told to expect.
function distinct(all: string[]): string[] {
  const seen = new Set<string>();
  const kept: string[] = [];
  for (const one of all) {
    const key = one.toLowerCase();
    if (seen.has(key)) continue;
    seen.add(key);
    kept.push(one);
  }
  return kept;
}
