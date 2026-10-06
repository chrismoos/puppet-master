import { computeKeyboardOverlap } from "./keyboardOverlap";

export interface KeyboardInsetState {
  inset: number;
  viewBottomY: number;
}

export type KeyboardAction =
  | { type: "show"; keyboardTopY: number; duration: number }
  | { type: "changeFrame"; keyboardTopY: number; screenHeight: number; duration: number }
  | { type: "willHide"; duration: number }
  | { type: "didHide" }
  | { type: "disable" };

export interface KeyboardInsetResult {
  state: KeyboardInsetState;
  toValue: number;
  hard: boolean;
  duration: number;
}

export function reduceKeyboardInset(
  prev: KeyboardInsetState,
  action: KeyboardAction,
): KeyboardInsetResult {
  switch (action.type) {
    case "show": {
      const overlap = computeKeyboardOverlap(prev.viewBottomY, action.keyboardTopY);
      return {
        state: { ...prev, inset: overlap },
        toValue: overlap,
        hard: false,
        duration: action.duration || 250,
      };
    }

    case "changeFrame": {
      const offScreen = action.keyboardTopY >= action.screenHeight;
      const overlap = offScreen
        ? 0
        : computeKeyboardOverlap(prev.viewBottomY, action.keyboardTopY);
      return {
        state: { ...prev, inset: overlap },
        toValue: overlap,
        hard: false,
        duration: action.duration || 250,
      };
    }

    case "willHide":
      return {
        state: { ...prev, inset: 0 },
        toValue: 0,
        hard: false,
        duration: action.duration || 250,
      };

    case "didHide":
      return {
        state: { ...prev, inset: 0 },
        toValue: 0,
        hard: true,
        duration: 0,
      };

    case "disable":
      return {
        state: { ...prev, inset: 0 },
        toValue: 0,
        hard: true,
        duration: 0,
      };
  }
}

export function initialKeyboardInsetState(): KeyboardInsetState {
  return { inset: 0, viewBottomY: 0 };
}
