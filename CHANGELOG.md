# Changelog

## Unreleased

- New `spawn.program_status` setting, off by default: agent terminals answer the Program Status Protocol (OSC 7501), and an agent that reports through it drives its session state ahead of lifecycle hooks, with its message and progress in the state detail.
- Interrupting a Codex turn immediately marks the session idle with an interruption reason.
- A session whose agent is killed on its host is resumed instead of left ended, up to three times, and says which signal ended it.
- A session ended by a signal reports 128 plus the signal, so a killed agent reads 137 rather than 1.
- [Web] Dropping a file on a session's terminal stores it on the host that runs that session and types the path into the composer, so an agent can open it whether it runs locally or on a remote host.
- [Web] A file dropped anywhere else in the dashboard is ignored instead of replacing the dashboard with the file.
- [Daemon] A host keeps files dropped onto its sessions for seven days. A host running a pm that predates this refuses a drop and says so.

- Switching terminals waits for the destination to render at the pane size before showing it.
- Browser tests select the installed test agent and verify scrollbar timers independently of runner speed.
- [iOS] Tapping a link in terminal output opens it, the key row keeps its keys in reach, and a sticky Alt modifier joins Shift and Ctrl.
- [iOS] The spawn sheet offers every agent and opens the session it creates, and clearing filters closes the filter bar.
- [iOS] A tapped forward link opens signed in for that forward, at any path under it, instead of on the sign-in page.
- Killing a session in the iOS app returns to the session list, where a session that just ended stays listed and can be resumed.
- The iOS session menu no longer offers Interrupt.
- [Web] Reading back through a terminal's scrollback while an agent prints no longer pushes the text upward: it stays where you scrolled.
- [iOS] Text you have scrolled up to read stays in place as output arrives, also once the terminal's scrollback is full.

## 0.10.0 — 2026-10-06

- Initial release.
