import { Alert, Linking } from "react-native";

import { isOpenableUrl } from "../links";

/**
 * Opens a URL, or says why it will not. Refusing silently reads as a broken
 * link, which is the one outcome that invites tapping it again.
 */
export function openExternalUrl(url: string): void {
  if (!isOpenableUrl(url)) {
    Alert.alert("Unsupported link", `Cannot open "${url}" — only http and https links open.`);
    return;
  }
  void Linking.openURL(url).catch((err) => {
    Alert.alert("Could not open link", err instanceof Error ? err.message : String(err));
  });
}
