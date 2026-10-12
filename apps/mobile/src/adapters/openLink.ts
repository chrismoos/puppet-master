import { Alert, Linking } from "react-native";

import { isOpenableUrl } from "../links";

type UrlResolver = (url: string) => Promise<string>;

let resolveUrl: UrlResolver | null = null;

/**
 * Installs what turns a tapped URL into the one to open, so a link into a
 * published forward opens signed in. Returns the uninstaller.
 */
export function setLinkResolver(resolver: UrlResolver): () => void {
  resolveUrl = resolver;
  return () => {
    if (resolveUrl === resolver) resolveUrl = null;
  };
}

/**
 * Opens a URL, or says why it will not. Refusing silently reads as a broken
 * link, which is the one outcome that invites tapping it again.
 */
export function openExternalUrl(url: string): void {
  if (!isOpenableUrl(url)) {
    Alert.alert("Unsupported link", `Cannot open "${url}" — only http and https links open.`);
    return;
  }
  const resolver = resolveUrl;
  void (resolver ? resolver(url).catch(() => url) : Promise.resolve(url))
    .then((target) => Linking.openURL(target))
    .catch((err) => {
      Alert.alert("Could not open link", err instanceof Error ? err.message : String(err));
    });
}
