# Puppet Master for macOS

A desktop window onto a Puppet Master controller, built with Tauri. It is a
client: it runs no controller or worker of its own, and opens the web UI your
controller already serves. Sign-in, the API and every update to the UI work
exactly as they do in a browser.

What the app adds over a browser tab:

- **Desktop notifications** for sessions that need you, finish or fail, with
  no browser permission prompt. While the window is focused, alerts show in
  the page as they do in a browser.
- **A Dock badge** counting the sessions waiting on you.
- **Windows for new tabs.** A page of your controller that would open in a
  new tab, such as a review, opens as another app window. Links to other
  sites open in your default browser.
- **Standard Edit shortcuts** (copy, paste, select all) inside the app.

## Native terminals

On macOS every terminal is drawn natively, the agent's as well as shell
tabs and pane terminals: a Metal view laid over the place the page keeps
for it, with libghostty-vt holding the screen and the
GPU drawing it, so heavy output never waits on the page. The view uses the
page's own cell size, so the grid matches the browser's, and it follows the
page's layout as panes move or the session list collapses.

- Click the terminal to type into it. Keys go through libghostty's key
  encoder, so application cursor keys and the kitty keyboard protocol work.
  ⌘V pastes, bracketed when the program asks. Other ⌘ shortcuts go to the
  app's menus.
- Input methods work: composing text (such as Japanese or accented
  letters) is drawn underlined at the cursor, with the candidate window
  beside it, and is sent once committed.
- Read-only panes take no typing, but still select and copy.
- Drag to select, double-click for a word, triple-click for a line, and ⌘C
  to copy. Text a program copies with OSC 52 lands on the clipboard.
- ⌘-click a link to open it: an OSC 8 hyperlink, or a URL or `pm:` link
  written in the text. Your controller's own pages open in the app, other
  sites in your default browser.
- A program that asks for the mouse, such as a full-screen editor, gets
  clicks, drags and the wheel. Hold Shift to select text instead.
- Otherwise the wheel and trackpad scroll back through history, and a right
  click opens the page's terminal menu.
- Drop files on the terminal to upload them, as in a browser.
- Page menus, dialogs, alerts and the pane's hover header show over the
  terminal.
- Recently viewed terminals stay attached while hidden, up to eight, so
  switching back to one is instant.
- A shell's tab follows the program running in it, as in a browser.
- It holds the page's only socket to the terminal, opened with a one-use
  ticket the page mints. It claims the terminal's size when it connects,
  when its area changes, and when you type into it after another viewer
  took the size.

The page's own terminal still runs underneath, without a socket, to give
the native view its cell size.

The terminal crate is `apps/desktop/terminal`. Its tests run on any host
with a GPU, including a software Vulkan driver:

```sh
cd apps/desktop && cargo test -p pm-native-terminal
```

## First launch

The app asks for your controller's address, the one you open Puppet Master at
in a browser, such as `https://pm.example.com`. A bare host name is taken as
`https://`. The address is remembered in the app's configuration directory,
and **Controller → Change Controller…** (⇧⌘K) returns to this page.

## Build and run

You need a Mac with Rust and Xcode's command line tools, and the Tauri CLI:

```sh
cargo install tauri-cli --version "^2" --locked
```

Then, from the repository root:

```sh
make desktop-dev     # run the app from source
make desktop-build   # build Puppet Master.app and a .dmg
```

`make desktop-build` writes the bundle under the crate's
`target/release/bundle/` (or under `CARGO_TARGET_DIR` when that is set). The
build is not signed or notarized yet, so macOS asks you to confirm opening it
the first time: Control-click the app and choose Open.

The crate is its own Cargo workspace, separate from the controller's, because
Tauri needs the platform web view's development libraries. On Linux that is
WebKitGTK (`libwebkit2gtk-4.1-dev`), which is enough to compile and test it.
