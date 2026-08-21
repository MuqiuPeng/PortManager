# Desktop app

The window over the same daemon the CLI and the MCP server talk to, which is why
a service an agent starts appears here without either of them knowing about the
other. What is specific to it is here; the daemon's own design is in
[architecture.md](architecture.md).

## Desktop app

`apps/desktop` is a Tauri 2 shell around a React frontend. Its Rust side holds a
`DaemonHandle` — a self-repairing IPC connection — and every `#[tauri::command]`
is a one-to-one translation of an IPC request. No state and no logic live in the
app: anything computed there would be something the CLI and MCP do not get.

Registry edits are announced too, not just lifecycle changes. Adding, correcting
or removing a service used to publish nothing, so a service an agent declared
through MCP did not appear in an open window until something unrelated happened
— which reads exactly like the edit not having worked.

A second connection carries the event subscription, so a long-lived stream never
interleaves with a command the user just clicked. Events are re-emitted to the
frontend on the `runtime://event` channel, which is what makes a service started
from a terminal appear in the window without polling.

Two things still poll, because the daemon has no event for them: the ports table
(the socket table changes without the runtime's involvement) and log tailing,
which uses the `since_seq` cursor so each tick transfers only new lines.

## Dialogs

The webview implements none of the JavaScript panel callbacks, so `alert`,
`confirm` and `prompt` do nothing and return null or false. Three controls were
built on them and were silently dead: adding a service, scanning a folder, and
removing a service — the last being a destructive action whose button appeared
to work and did not.

Anything that needs an answer from the user is an in-app sheet. Confirmation is
a two-step button rather than a dialog: the first click arms it, the second
performs it.

## Windows and activation

The activation policy is not fixed: it follows the main window. `Regular` while
the window is on screen, `Accessory` — no Dock icon, no ⌘-Tab entry — once it is
closed.

Pinning it to `Accessory` seems right for a menu-bar app but is wrong in a way
that is hard to attribute: macOS withholds full-screen support from accessory
apps, so the window launched at startup had no full-screen button while the
same window reopened from the tray did, because reopening switched to `Regular`
on the way. Two windows that were never actually two windows.

Closing the main window **hides** it rather than destroying it. Tauri's default
is to destroy, after which `get_webview_window("main")` returns `None` and the
tray's "Open main window" silently does nothing — the window is gone for the
rest of the session.

Reopening switches the activation policy to `Regular` **before** showing and
focusing. An accessory application cannot bring a window to the front, so
focusing first leaves it behind whatever the user was in. Hiding switches back,
so the Dock icon does not outlive the window that justified it.

## The edge panel

The panel's defining property is that clicking it does not take focus from the
editor. On macOS that is `NSWindowStyleMask::NonactivatingPanel`, which the
window server honours only for `NSPanel` — and Tauri creates an `NSWindow`. So
`adapter-macos` swaps the class at runtime, then verifies the style mask took;
a silently failed adoption looks fine at startup and only surfaces later as a
panel that steals focus on every click.

It also joins all Spaces and sits at `NSStatusWindowLevel`, so it does not
vanish when the user switches Space or opens a full-screen app.

The panel is never absent. At rest it is a slim tab against the screen edge, so
expanding is a *resize* rather than an appearance — which makes it discoverable
(an invisible hover strip is something you have to be told about) and gives the
expansion something to animate, since there is already a window on screen.

```
island ──pointer reaches the tab──▶ expanded (passive: keeps the editor's focus)
  ▲                                    │
  └────pointer leaves the panel────────┘
island ──shortcut / menu bar──▶ expanded (focused: keyboard works)
pinned ────────────────────────▶ expanded always
```

The tab is click-through while resting (`setIgnoresMouseEvents`), so a permanent
strip at the screen edge never swallows a click meant for the window underneath.
That costs nothing, because proximity is found by polling the pointer rather
than by receiving events.

The window itself is transparent and the panel draws its own rounded background;
without that the window paints an opaque rectangle and the rounded corners show
as white squares. On macOS that needs Tauri's `macos-private-api` feature, which
rules out the Mac App Store — not a distribution channel for a tool that manages
local processes anyway.

The distinction between passive and focused is the reason the panel exists in
this form: a pointer reveal must not disturb what you are typing into, while a
deliberate keystroke should, or you cannot type into what you just summoned.
A panel revealed by hovering is also not "already open" as far as the shortcut
is concerned — pressing it focuses rather than dismisses, or the key appears to
do nothing.

Settings live in the daemon's `settings` table as an opaque JSON blob. The
geometry means nothing to the daemon, but keeping it there is what makes it
survive reinstalling the bundle, and leaves one answer to "where is the state"
instead of two. A blob it cannot parse — an older layout — falls back to
defaults with a warning rather than blocking the settings screen.

Rebinding the shortcut registers the new accelerator *before* releasing the old
one, so a combination another app already owns is refused with the previous one
still in force.

Edge detection polls the pointer every 80ms on the main thread. The alternative,
a global `CGEventTap`, would demand Accessibility permission for something this
small; polling asks for nothing and is imperceptible.

## Failures

A service that stops working raises a toast in the corner rather than a banner
in the page. A banner pushes everything down at the moment the layout should be
holding still — the row somebody was about to click has moved — and the corner
leaves the page where it was.

They do not fade. A toast that disappears is right for "saved" and wrong for
"your API is down": what makes an error worth showing is that somebody has to
act on it, and it should still be there when they look back. Each is dismissed
on its own, since reading one is not reading the rest, and a dismissal is
forgotten once that service stops failing — so the same thing breaking again is
shown again.

Every one can be copied, and so can a service's whole log. The next thing that
happens to an error message is that it gets pasted somewhere: a search, an
issue, a message to whoever owns the service. The text stays selectable for
anyone who wants one line instead of all of them.

Copying goes through the clipboard plugin rather than `navigator.clipboard`.
The app is served from a custom scheme, which is not a secure context, and the
browser clipboard API is absent there — the same shape as `prompt`, which this
app has already learned about once. The capability grants writing only; there is
no reason for this app to read what somebody else copied.
