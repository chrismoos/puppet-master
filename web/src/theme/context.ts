import { createContext, useContext } from "react";
import type { TerminalThemeController } from "./controller";

export const TerminalThemeContext = createContext<TerminalThemeController | null>(null);

export function useTerminalThemeController(): TerminalThemeController {
  const controller = useContext(TerminalThemeContext);
  if (!controller) throw new Error("TerminalThemeContext missing");
  return controller;
}
