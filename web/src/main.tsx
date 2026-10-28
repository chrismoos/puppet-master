import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import "@fontsource-variable/inter";
import "@fontsource-variable/inter/wght-italic.css";
import "@fontsource-variable/jetbrains-mono";
import "@fontsource-variable/jetbrains-mono/wght-italic.css";
import "./styles.css";
import "./controls.css";
import { App } from "./App";
import { preloadTerminalFonts } from "./fonts";
import { guardStrayFileDrops } from "./fileDropGuard";

function renderApp(): void {
  guardStrayFileDrops(document);
  createRoot(document.getElementById("root")!).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

// Load bundled faces before measuring terminal cells.
void preloadTerminalFonts().then(renderApp);
