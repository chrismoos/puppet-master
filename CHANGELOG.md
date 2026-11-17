# Changelog

## Unreleased

- [Web] Clicking a terminal URL excludes closing parentheses that surround the link while preserving parentheses in the URL.

- [Daemon] New remote agent terminals open without a five-second history timeout during startup.

## 0.11.0 — 2026-11-16

- [Web] Switching to a preloaded session after reload preserves its terminal size when session headers have different heights.

- [Web] Fullscreen terminal applications select the text under the pointer when fonts have fractional character widths.

- [Web] Returning to a scrolled session keeps the same text visible when output arrives during the switch.

- [Daemon] Reading or waiting on a terminal pane keeps working when its program exits during the read.

- [Web] Active sessions preload recent terminal content in recently viewed order, with bounded memory and older scrollback fetched as you scroll.

- [Daemon] Grok Build sessions can be spawned and resumed alongside the other agents, with dashboard reporting, needs-input detection and permission modes. They always use Grok's fullscreen interface, a resumed one keeps the instructions it started with, and a remote worker launches them once it runs this release.

- [Web] Terminal text keeps consistent top padding when scrollback fills or pane height changes.

- [Web] A shell opened from the pane menu receives keyboard focus when it is ready.

- [Web] Opening a shell pane no longer triggers continuous canvas growth and resize work that can freeze the page.

- [Daemon] Video and audio in a published directory can be seeked: files are served in byte ranges when the browser asks for part of one.

- [Web] Session switches hide the outgoing terminal before header and directory bars change its height, then reveal only the selected terminal at its final size.

- [Web] Terminal glyphs keep their pixel size while a pane changes size and its replacement snapshot is pending.

- [Web] Session switches and reloads keep stale-size terminal snapshots covered.

- [Web] Resizing inline agent terminals keeps the screen visible while the program clears and redraws it.

- [Web] Streaming terminal output stays current while programs redraw their prompts, without waiting for later batches or repainting the whole screen for every update.

- [Daemon] Each prompt you send a Claude Code, Codex or Gemini session tells the agent what its dashboard goal and headline say and how long ago it set them, so it updates them when your request moves on, and an hour-old report is called stale.

- [Web] Page and browser panes can be closed, not just hidden. Pane headers show an × at the far right in place of the stop square, and closing a terminal pane still asks first.

- [Web] Agents can browse with managed Playwright tools and show the same browser live in a read-only pane, with isolated login contexts and automatic browser cleanup.

- [Web] Alert cards hide on their own again after you dismiss or open the last card with the pointer still on it. Before, every later card stayed until you pointed at the stack.

- [iOS] Logging out now keeps you logged out after the app is closed. It forgets the device's credentials on the phone even offline, revokes the device on the controller when it can so notifications stop, and clears delivered notifications and the badge.

- [iOS] The launch screen and the cover shown in the app switcher use the app's own dark background with the logo on it, instead of a white or black screen and a logo in a mismatched box. Menus open just below their ⋯ button.

- [iOS] Bucket tiles across the top of the session list, with each bucket's icon and the web rail's indicators, narrow it to one bucket, and tapping the selected bucket again lists every bucket.

- [iOS] In a session with panes, swipe left or right with two fingers to move between the agent, its shell panes and page panes, and double-tap the header to go back to the agent. Dots in the header show which pane is on screen, ⋯ has Agent and New shell pane, and Terminals groups the agent, panes and shell tabs.

- [Daemon] A shell you open in a pane is your own: opening it, stopping it, typing in it or exiting it no longer sends the agent a message. Hiding or restoring any pane no longer messages the agent either.

- [macOS] A desktop app opens your controller in its own window, with no title bar or native window buttons, desktop notifications, a Dock badge for sessions waiting on you, and new windows for pages that would open in a new tab. The window has its own close, minimize and full screen buttons on the top bar. Every terminal is drawn natively with Metal in your terminal theme, with selection, copy, ⌘-click links, input methods, mouse support for programs that ask for it, file drops, page zoom, connection status, a scrollbar, VoiceOver, and instant switching back to recent terminals. A terminal another device resized fits the window again as soon as you type, click, scroll or move the pointer over it. Build it with `make desktop-build`.

- [Web] Pages an agent opens in a pane follow Puppet Master's dark or light appearance instead of always rendering light.

- [Web] While you are looking at Puppet Master, alerts show as cards stacked at the top right instead of desktop notifications, each with an icon for the kind of alert, the session's name and its question. Click a card to open its session or × to dismiss it. Each card hides on its own unless the pointer rests on the stack, and up to three show at once. Alerts still go to the desktop while the tab is in the background.

- [Daemon] When you ask an agent to tell you about something, it can notify you with `notify_user`: a card in the dashboard, or a desktop or phone notification when you are away. A notification can link to a page or preview the session published, or to a board item. Each session can send one every 10 seconds.

- [Daemon] A session's question stops showing as needs-input once you answer it in the terminal, Codex included, or once its agent goes back to work, instead of lasting until the agent's turn ends.

- [Web] Right-click an agent's terminal, a pane's header or the agent's hover header to open a new shell beside it, in any direction. A shell tab splits the same way, and the shells inside it close with the tab. Every live session shows the hover header, including one with no panes yet.

- [Web] A shell or program pane nobody named shows the program it is running, such as bash or vim, until it is given a title.

- [Daemon] A shell pane closes when its shell exits.

- [Web] A bucket with a session mid-turn shows a blue dot on its rail button, and bucket letters on the rail are larger.

- [Web] Drag a pane by its header, or the agent by the header that shows when you hover its top edge, to move it beside another pane or swap the two. Double-click a header to zoom that pane or the agent over the others, and press Esc to restore.

- [Web] The saved workspaces tab bar is hidden. Saved workspaces still open from their links.

- [Web] The session list collapses to the bucket rail with the button at the top of the rail, or with ⌘\ (Ctrl+Shift+\ elsewhere), and stays collapsed across reloads. The rail no longer has a Needs you button, All buckets shows a grid icon, and search is a quiet magnifier under the collapse button.

- [Web] A terminal changes size in the same frame as the layout around it when you collapse the session list, collapse or expand a pane, or resize the window, and an agent's redraw at the new size lands with the change instead of jumping in afterwards.

- [Web] Agents can open panes beside their terminal, showing a live program, a shell, an HTML page from their working directory or a dev server, and arrange them in splits. You can resize, collapse, hide and restore them, or stop a program, and the agent is told. Starting programs requires bypass permissions.

- [Web] Connections, connection calls and approvals update as soon as they change instead of every few seconds, and an open tab no longer polls for them. The iOS approvals list and badge do the same. A pending approval that runs out shows as expired without a reload.

- [Daemon] A review you finished stays finished. Notices about review comments wait until the agent's turn ends and are dropped if you finished the review or resolved the comments meanwhile, a notice is never delivered twice, and an agent cannot reopen a finished review without saying so explicitly.

- [Web] A bucket picture with rounded or transparent corners no longer shows a grey wedge and frame at each corner of its rail button. The button behind a picture is see-through, and hover, the current bucket and needs-input draw a ring around it.

- [iOS] The app builds with Expo SDK 57 and React Native 0.86 and uses the scene-based app life cycle, so it launches when built with Xcode 27 and `make ios-run` finds the Simulator, which Xcode 27 renamed Device Hub.
  Migration: building the iOS app now needs a Swift 6.2-era Xcode or newer. Run `pnpm install`, then `make ios-reset` once to regenerate the iOS project.

- [iOS] The phone no longer takes a terminal's size while the app is in the background, which could happen after a push or when iOS launched the app on its own.

- [iOS] Returning to the app, touching the terminal or typing in it takes the terminal's size back from another screen, without the Update prompt.

- [Web] When you come back to the browser after another screen resized a terminal you can see, the terminal fits this window again on your first click, key, scroll or mouse movement, without the Update prompt. Coming back means the tab was hidden, the window lost focus, a minute passed without input, or another screen took the terminal.

- [Daemon] A Codex session shows needs-input when it asks you a question, not only for an approval.

- [Daemon] A session no longer stays on needs-input after you answer its approval prompt. It returns to working when the agent carries on, and to idle when its turn ends. Codex sessions were most affected.

- [Web] A worker's question goes to its supervisor instead of flagging you. While the supervisor runs, the question is left out of the rail, the bucket counts, the needs-you list and the tab title, and its row reads "waiting on its supervisor". It counts for you again if the supervisor ends.

- [Daemon] A supervisor is no longer woken about a worker it already saw through wait_sessions, or about one it killed itself.

- [Web] The sidebar shows buckets as a rail of buttons by default. Settings → Appearance switches back to the dropdown, and an account that already chose one keeps it.

- [Web] Rail buttons are a little larger, with bigger letters.

- [Daemon] Supervisors can set their bucket's rail icon when you ask, with letters or an emoji, a color, or a picture file from their working directory, and can read it back. Only a short description comes back unless the agent asks to see the picture.

- [Web] Terminals run on libghostty-vt 1.0.1, which uses less memory after a terminal is restored from its worker. Workers on the previous version keep working and need no update.

- [Web] A terminal whose program left synchronized output on now leaves it after the timeout, instead of redrawing only every half second and then crashing when the program next sent a progress report.

- [Web] The sidebar can list one bucket at a time. A switcher at its top picks a bucket, every bucket, or only the sessions that need you, and shows how many sessions in other buckets need you. Settings → Appearance can show the buckets as a rail of buttons beside the sessions instead, saved to your account. Rail buttons show letters that differ for every bucket, and a bucket's settings can replace them with your own letters or emoji, a color, or an uploaded picture. In a single bucket, sessions idle for over a day fold into an Older row, while at least five recent sessions stay listed.

- [Web] On the rail, switching to a bucket reopens the session you last had open there, or its first session. A bucket's button shows an amber ring when a session needs you, pulsing until you have seen it, and a green dot for a finished turn you have not viewed. The selected bucket is marked with a bar beside its button instead of amber.

- [Web] Session search moved into the bucket switcher: press `/`, open the switcher, or use the rail's search button, then type to find a bucket or a session in any bucket, or show every result in the sidebar. The search box no longer takes a row of its own.

## 0.10.4 — 2026-11-08

- [Web] A terminal that is not live says why in proportion to what it needs from you: a pill while it reconnects, a banner with Retry now or View worker while its connection or worker is down, and a card naming the fix (update the worker or reload the page) when its terminal engine doesn't match.

- [Web] A session terminal follows the window on every frame of a resize drag instead of holding its old grid until the session answers at the new size, and its text no longer bounces while the window height changes.

- [Daemon] Agents can pass connection call arguments as `arguments_path`, a JSON file in their working directory read once when the call is submitted, instead of inline `arguments`. Later edits to the file cannot change a submitted call, and a large argument set is not repeated back to the agent.

- [Web] A call with large arguments, in Approvals and in Connections, shows where they came from, their size and SHA-256, an outline of their fields and their first lines, with Show all and a download of the exact JSON.

- [Daemon] OAuth connections accept a token whose endpoint names a nonstandard token type and send it as a bearer token, instead of failing with "Unsupported OAuth token type". DPoP, MAC and N_A tokens are still refused, now by name.

- [Daemon] A connection call whose result is over 32 KiB returns a preview and the path of a file holding the full result on the session's host, instead of the whole result or a storage-limit error. A result over 10 MiB keeps its first 10 MiB and still reports whether the call succeeded.

- [Web] An open terminal keeps growing its scrollback while output streams instead of stopping at the worker's 5000 lines after it connects or reconnects.

- [Web] OAuth connections can use the client credentials grant: enter the token URL, client ID and client secret, and Puppet Master fetches and renews tokens itself with no sign-in.

- [Web] Show gateway errors during agent installation instead of a JSON parsing error.

- [Web] Restore terminals with the same Ghostty engine used on their worker, preserving both screens and in-progress output across reconnects.
Migration: Update workers with the controller before opening their terminals, and reload older browser tabs.

- [Web] Draw terminal block and shade characters as seamless cell graphics, matching native Ghostty progress bars.

- [Web] Keep terminal colors and text styles intact after resizing or reconnecting.
- [Web] Switch browser terminals from xterm.js to Ghostty with Canvas 2D rendering.
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
- [Web] A refused connection test says what it asked the API for and what the API answered, with the status code and the start of the response, rather than one line naming the endpoint and the credentials. The daemon logs the same detail.
- [Web] A read operation that answers tests a REST connection even when its base URL refuses, and the connection reads as tested from then on.
- [Web] Sessions kept warm in the background no longer draw their terminals while hidden. With several busy agents, each drew every frame, which held the page and the GPU and made switching sessions take seconds.
- [Web] Clicking a session takes the terminal pane at once, covered by a loading curtain in the terminal's own colors until its screen arrives at the pane's size, instead of keeping the previous session up or flashing its old screen while it redraws. A session whose agent kept printing while its size changed could leave the previous session on screen for seconds, or until it went quiet, and an ended session showed the previous session's terminal behind its card.
- [Web] Pressing a session in the sidebar starts attaching its terminal before the click lands, so a session not opened recently paints sooner.
- [Web] A review on the working tree marks the agent's newest round as read once the diff shows it, so the "Rev N ready" banner no longer asks you to advance to a diff already on screen. A finished round also reloads the diff straight away. The diff holds still while a comment or reply box is open on any file, or an unsent comment sits on a file that changed, and offers the round instead. The banner also stays while you are reading an earlier round. Banners appearing and leaving no longer move what you are reading.
- [Web] Sending a review comment or reply closes its box at once. A box opened while the send was still in flight no longer closes when the send finishes, and a failed send puts the text back.

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
