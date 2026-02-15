# macOS server cursor suppression: problem and next approach

## Problem statement
When control transitions from a macOS server to a Windows client, the macOS cursor can remain visible and local clicks can still affect the macOS desktop.

Expected behavior:
- Cursor should no longer be operable on the macOS server while remote control is active.
- Local mouse/keyboard input should be redirected to the remote client, not applied locally.

Actual behavior:
- Cursor visibility is inconsistent (can stay visible).
- Local click side effects can still happen on macOS during remote control.

## What exists today
- Transition state is toggled correctly in `src/main.rs` via `input_capture.set_suppress(true/false)`.
- macOS suppression currently relies on `set_cursor_suppression()` in `src/input/macos.rs`:
  - `CGAssociateMouseAndMouseCursorPosition(false/true)`
  - `CGDisplayHideCursor` / `CGDisplayShowCursor`
- macOS event tap path currently uses a listen-only tap and only captures mouse move/drag events.

## Why this still fails
1. Hiding the cursor is not the same as suppressing local input.
   - Even with cursor hide/disassociate calls, local clicks are not reliably blocked.
2. The event tap is listen-only.
   - A listen-only tap cannot swallow events, so macOS still receives local click/key events.
3. Capture is movement-focused.
   - Current macOS tap does not capture button/scroll/keyboard events for forwarding while remote is active.
4. Cursor hide is display-scoped.
   - Calling hide/show against main display may not fully cover multi-display pointer visibility cases.

## Next approach (chosen)
Shift suppression from cursor-visibility tricks to event-level ownership:

1. Use a non-listen-only CGEventTap on macOS for input events we care about.
2. Expand the event mask to include:
   - Mouse move + drags
   - Mouse button down/up
   - Scroll wheel
   - Key down/up (+ flags changed if needed for modifiers)
3. Thread the shared `suppressing` flag into the tap callback context.
4. While `suppressing == true`:
   - Convert events to `InputEvent` and send them to the existing channel.
   - Return null from callback for suppressible events so macOS does not handle them locally.
5. While `suppressing == false`:
   - Keep normal local behavior (return event unchanged).
6. Keep cursor hide/show as a best-effort visual signal, but make it secondary to event suppression.
   - Improve display targeting for hide/show diagnostics on multi-display setups.

## Why this is the best next move
- It directly addresses the real bug: local events are not being blocked.
- It matches the architecture already used in the server loop (capture -> protocol -> remote inject).
- It avoids depending on fragile cursor-visibility behavior as the primary control mechanism.

## Implementation scope
Primary files:
- `src/input/macos.rs` (event tap mode, callback logic, suppression semantics)
- Possibly small wiring updates in `src/main.rs` only if additional state handling is needed

No protocol changes required for this step.

## Acceptance criteria
- When server enters remote control state, local macOS clicks do not activate local UI.
- Cursor is not visibly interactive on macOS while remote control is active.
- Mouse/keyboard events generated on macOS during remote control are forwarded to and observed on Windows client.
- Returning control to macOS restores normal local input and cursor visibility.

## Validation plan
- Manual test: macOS server -> Windows client transition on single-display setup.
- Manual test: same flow on multi-display macOS setup.
- Verify logs around `set_suppress` transitions and tap callback suppression decisions.
- Run checks after implementation:
  - `cargo build`
  - `cargo test`
  - `cargo fmt --all`
  - `cargo clippy -- -D warnings`
