import { useEffect, useState } from "react";
import { parseRoute, settingsRoutePath, type Route } from "@puppet-master/client-core/router";

export {
  SETTINGS_CATALOGS,
  SETTINGS_GROUPS,
  SETTINGS_SECTIONS,
  parseRoute,
  selectedSessionId,
  sessionHomeId,
  settingsRoutePath,
  type Route,
  type SettingsCatalog,
  type SettingsSection,
  type SettingsTarget,
} from "@puppet-master/client-core/router";

export function navigate(path: string): void {
  location.hash = path;
}

/**
 * Rewrites the URL without adding a history entry.
 *
 * For a move the back button should not have to undo: reconciling the
 * URL with where the reader already is, rather than taking them
 * somewhere new. Deliberately does not fire `hashchange`, so the page
 * that asked for it does not hear its own write back.
 */
export function replaceRoute(path: string): void {
  history.replaceState(history.state, "", `#${path}`);
}

/**
 * The route the address names. An old Settings or Manage address is
 * rewritten to the page's one canonical address on the way, without a
 * history entry, so a bookmark keeps working and Back never returns to it.
 */
function readRoute(): Route {
  const route = parseRoute(location.hash);
  if (route.name === "settings") {
    const canonical = settingsRoutePath(route.section, route);
    if (location.hash !== `#${canonical}`) replaceRoute(canonical);
  }
  return route;
}

export function useRoute(): Route {
  const [route, setRoute] = useState<Route>(readRoute);
  useEffect(() => {
    const onChange = () => setRoute(readRoute());
    window.addEventListener("hashchange", onChange);
    return () => window.removeEventListener("hashchange", onChange);
  }, []);
  return route;
}
