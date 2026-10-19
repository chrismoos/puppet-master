# Changelog

## Unreleased

- [Web] Talk to Puppet Master from the topbar: ask what needs you, what a session is doing, or tell it to start work, across every bucket you can see. Answers are read aloud when a speech provider is configured, and typing does the same thing.
- [iOS] A voice screen in the overflow menu, with tap to talk or a call that keeps the microphone open and sends what you say as you say it. Speech is transcribed on the device, so no audio leaves the phone, and answers come out of the speaker or the earpiece, whichever you pick.
- [Daemon] A message can be handed to any session's agent as the user, and the short answer it sends back is read out in the agent's own words.
- [Daemon] New voice settings choose the model profile the voice agent reasons with and the provider that renders its speech, under `voice.model.profile` and `voice.tts.*`. The credential is sealed at rest and never read back.
- [iOS] A token refresh that is slow to answer no longer logs the device out. The app waits for the attempt it already made instead of asking again with the same token, which the daemon treats as a stolen one.
- [iOS] The connection header says which credential the daemon refused, with its status code and the daemon's own reason, instead of only reporting that access was revoked. It also reports a 401 that did not come from the daemon as a connection problem rather than dropping the device's credentials for it, and says so when the daemon's clock trails the phone's, which expires tokens on arrival.
- [iOS] A short `mobile.access_token_ttl_minutes` no longer makes the app refresh the moment it stores a token: the refresh lead is bounded by the lifetime the daemon issues.
- [Daemon] A refused device token refresh now says why in the log: an unknown token, a device that is gone or already revoked, an expired token, or reuse. A storage failure during a refresh answers as a server error rather than as a bad token, so it no longer costs the device its credentials.
- [Web] Copying a terminal selection no longer carries the spaces an agent paints across the rest of each row, so pasting several lines keeps its shape. Selecting past the last line of output no longer appends the blank rows below it, and a selection holding only blank space leaves the clipboard as it was. An agent's own OSC 52 copy is still written exactly as sent.
- [Web] A REST connection activates without passing a test, after a prompt saying it was not verified, because its operations come from the OpenAPI document rather than from the test. It reads as active and unverified until a test passes, and an MCP connection still waits for the test that discovers its tools.
- [Web] The connection test offers the read operations the OpenAPI document reports, with the one that needs no arguments ready to run, instead of only the operations already classified as read by hand.
- [Daemon] Editing a connection's OpenAPI document reimports its operations straight away, so its tools match the document it was saved with.

## 0.10.3 — 2026-10-14

- [Web] The build stamp in the topbar shows the version alone, with the git rev in its tooltip, and turns into a button when a newer build is published. It opens a dialog with the command that fetches the update and the restart that applies it.
- [Web] New Settings page, About, with the build this controller runs, the channel it follows, the newest published version, and when the daemon last looked.
- [Daemon] `/api/version` reports when the release manifest was last read, so the dashboard can say how old the published version beside it is.

- New `spawn.program_status` setting, off by default: agent terminals answer the Program Status Protocol (OSC 7501), and an agent that reports through it drives its session state ahead of lifecycle hooks, with its message and progress in the state detail.
- Interrupting a Codex turn immediately marks the session idle with an interruption reason.
- A session whose agent is killed on its host is resumed instead of left ended, up to three times, and says which signal ended it.
- A session ended by a signal reports 128 plus the signal, so a killed agent reads 137 rather than 1.
- [Web] Dropping a file on a session's terminal stores it on the host that runs that session and types the path into the composer, so an agent can open it whether it runs locally or on a remote host.
- [Web] A file dropped anywhere else in the dashboard is ignored instead of replacing the dashboard with the file.
- [Daemon] A host keeps files dropped onto its sessions for seven days. A host running a pm that predates this refuses a drop and says so.
- [Web] A session whose terminal has not loaded yet takes the pane empty, with a connecting note once the wait is noticeable, instead of leaving the previous session's screen up.
- [Web] Opening a session paints its screen from a few hundred lines of history, about nine times sooner than from the whole scrollback, and reading back to the top fills in the rest.

- Switching terminals waits for the destination to render at the pane size before showing it.
- Browser tests select the installed test agent and verify scrollbar timers independently of runner speed.
- [iOS] Tapping a link in terminal output opens it, the key row keeps its keys in reach, and a sticky Alt modifier joins Shift and Ctrl.
- [iOS] The spawn sheet offers every agent and opens the session it creates, and clearing filters closes the filter bar.
- [iOS] A tapped forward link opens signed in for that forward, at any path under it, instead of on the sign-in page.
- Killing a session in the iOS app returns to the session list, where a session that just ended stays listed and can be resumed.
- The iOS session menu no longer offers Interrupt.
- [Web] Reading back through a terminal's scrollback while an agent prints no longer pushes the text upward: it stays where you scrolled.
- [iOS] Text you have scrolled up to read stays in place as output arrives, also once the terminal's scrollback is full.
- [iOS] Terminal text can be selected with a long press, extended by dragging its handles, and copied from the menu or the key row, without breaking link taps or losing the selection to streaming output.
- [iOS] Holding the keyboard's delete key keeps deleting.
- [iOS] The key row adds Home, End, PgUp and PgDn, which take Shift, Alt and Ctrl like the arrows.
- [iOS] The jump-to-bottom button appears whenever the terminal is scrolled up, idle or streaming, and for a full-screen agent scrolled up inside itself, where it sends Ctrl+End to Claude Code.
- [iOS] Opening a session shows its screen in one step, without a blank screen and stray cursor before the replay, the text scrolling past during it, or the agent repainting just after it.
- [iOS] Showing or hiding the keyboard keeps the terminal's picture steady while the rows change and the agent repaints, then switches to the repainted screen at once, instead of blanking and passing through intermediate layouts.

## 0.10.0 — 2026-10-06

- Initial release.
