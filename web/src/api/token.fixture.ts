import { accessToken, forgetAccessToken } from "./token";

/**
 * Seeds the module-scoped access token, so a test that stubs `fetch` sees only
 * its own request.
 *
 * Every authenticated call mints a token when it holds none, which would
 * otherwise spend the stub on the exchange and leave the test asserting against
 * the wrong call. Answers the mint from here and restores whatever `fetch` was
 * in place, so it does not matter whether a test calls this before or after
 * installing its own stub.
 */
export async function seedAccessToken(): Promise<void> {
  forgetAccessToken();
  const outer = globalThis.fetch;
  globalThis.fetch = (async () =>
    new Response(
      JSON.stringify({ accessToken: "seeded-test-token", expiresAtUnixMs: Date.now() + 600_000 }),
      { status: 200 },
    )) as typeof fetch;
  try {
    await accessToken();
  } finally {
    globalThis.fetch = outer;
  }
}
