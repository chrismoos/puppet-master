/** Decides whether a reconnecting status should trigger a bearer re-init.
 *
 *  The header provider already presents the current token on every retry,
 *  so a re-init is only needed when the access token actually changed
 *  (i.e. was refreshed because it was within the expiry slack). A
 *  gratuitous re-init tears down the wrapper's socket and resets its
 *  exponential backoff, so the app would open a new socket about every
 *  second while the daemon is unreachable. */
export async function shouldBearerReInit(
  currentToken: () => string | null,
  freshToken: () => Promise<string>,
): Promise<boolean> {
  const before = currentToken();
  const after = await freshToken();
  return after !== before;
}
