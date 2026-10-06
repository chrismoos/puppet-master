/** Identifies one manual refresh; 0 means none is outstanding. */
export type RefreshToken = number;

export interface ManualRefreshState {
  readonly outstanding: RefreshToken;
  readonly nextToken: RefreshToken;
}

export const idleRefresh: ManualRefreshState = { outstanding: 0, nextToken: 1 };

export function isRefreshing(state: ManualRefreshState): boolean {
  return state.outstanding !== 0;
}

/**
 * Starts a refresh unless one is already outstanding, so a second pull
 * cannot issue a second request whose answer would clear the wrong spinner.
 * Returns null when the pull is ignored.
 */
export function beginRefresh(
  state: ManualRefreshState,
): { state: ManualRefreshState; token: RefreshToken } | null {
  if (state.outstanding !== 0) return null;
  return {
    state: { outstanding: state.nextToken, nextToken: state.nextToken + 1 },
    token: state.nextToken,
  };
}

/**
 * Ends the refresh the token identifies, whether the request succeeded or
 * failed. An answer for a refresh that already ended leaves the state alone.
 */
export function endRefresh(state: ManualRefreshState, token: RefreshToken): ManualRefreshState {
  if (token === 0 || state.outstanding !== token) return state;
  return { ...state, outstanding: 0 };
}
