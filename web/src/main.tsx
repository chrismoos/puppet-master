import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/inter/wght-italic.css";
import "@fontsource-variable/jetbrains-mono";
import "@fontsource-variable/jetbrains-mono/wght-italic.css";
import "@xterm/xterm/css/xterm.css";
import "./styles.css";
import "./controls.css";
import { App } from "./App";
import { preloadTerminalFonts } from "./fonts";

function renderApp(): void {
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

// xterm measures glyphs when it opens, so load the bundled faces before any
// terminal can be constructed.
void preloadTerminalFonts().then(renderApp);
