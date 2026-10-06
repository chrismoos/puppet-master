export type FocusModeAction = "toggle" | "exit" | "enter";

export function focusModeActionForKey(key: string): FocusModeAction | null {
  return key === "Escape" ? "exit" : null;
}

export function focusModeReducer(active: boolean, action: FocusModeAction): boolean {
  switch (action) {
    case "toggle":
      return !active;
    case "enter":
      return true;
    case "exit":
      return false;
  }
}
