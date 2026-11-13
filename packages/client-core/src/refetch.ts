/**
 * Wraps a fetch so a call made while one is running runs once more after it,
 * instead of being dropped. A change announced mid-fetch may not be in that
 * fetch's answer, and with no poll nothing else would pick it up.
 */
export function coalescedRefetch(fetch: () => Promise<void>): () => Promise<void> {
  let running: Promise<void> | null = null;
  let again = false;
  const run = (): Promise<void> => {
    if (running) {
      again = true;
      return running;
    }
    running = (async () => {
      try {
        do {
          again = false;
          await fetch();
        } while (again);
      } finally {
        running = null;
      }
    })();
    return running;
  };
  return run;
}
