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

Terminals render the same way they do in the browser for now.

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
