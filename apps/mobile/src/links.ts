/**
 * Whether a URL is one this app will hand to the system.
 *
 * Links reach the app from agent output: a terminal's own link detection, and
 * markdown an agent wrote. iOS routes a URL to whichever app claims its scheme,
 * so anything other than the web is a way for agent output to reach another
 * app's handler. http and https are what a link in that output means.
 *
 * Kept free of React Native imports so it can be tested, with the opening
 * itself in the adapter beside it.
 */
export function isOpenableUrl(url: string): boolean {
  return /^https?:\/\//i.test(url.trim());
}

/**
 * Whether a controller URL carries the session unprotected.
 *
 * App Transport Security is off for every host in this app, and it has to be:
 * the controller is whatever address the operator typed, the daemon serves
 * plain HTTP by default, and the terminal's own WebSocket is opened from inside
 * the WebView, so narrowing the exception to web content would break the
 * terminal rather than the plaintext. The app cannot make that choice safe, so
 * it says when the choice has been made.
 */
export function isPlaintextController(baseUrl: string): boolean {
  return /^http:\/\//i.test(baseUrl.trim());
}

/** What the app shows when it is. */
export const PLAINTEXT_CONTROLLER_WARNING =
  "This controller is reached over plain http, so your sign-in, your device token " +
  "and everything in your terminals cross the network unencrypted. Use https, or " +
  "reach the controller over a VPN or tailnet.";
