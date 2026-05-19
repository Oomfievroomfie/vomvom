# vomvom

Rust programming text editor. Two core systems:

1. **Rendering engine** — custom HTML/CSS-like layout (cascade, selectors, flex/block), painted with femtovg (OpenGL). See `src/render/`.
2. **Hot exit** — SQLite-backed session (buffers + undo/redo), syncs every 5 min, autoloads on boot. See `src/session/`.

## Key facts

- Windows, PowerShell for system commands, bash-style paths for Bash tool, Windows-style paths (`C:\...`) for PowerShell tool. Never mix them.
- `cargo run --release -- --screenshot` renders to `screenshots/screenshot.png` (headless, no window)
- 60 tests: `cargo test`
- Font: `OpenSans-Medium.ttf` in project root (embedded via `include_bytes!`)

## Architecture

- `src/render/style.rs` — CSS-like cascade, selectors, specificity
- `src/render/layout.rs` — block/flex layout; local-space coords, `finalize_positions()` converts to absolute
- `src/render/paint.rs` — femtovg paint pass
- `src/render/femtovg_measurer.rs` — real text measurement for layout
- `src/session/db.rs` — SQLite schema + CRUD (WAL mode, bundled sqlite)
- `src/session/buffer.rs` — in-memory buffer + undo/redo ops
- `src/session/mod.rs` — Session: owns buffers, drives sync
- `src/main.rs` — winit event loop + demo scene; `AppState` owns all live state
- `src/headless_gl.rs` — shared GL context setup for headless paths
- `src/screenshot.rs` — headless single-frame screenshot
- `src/replay.rs` — scripted input replay for headless debugging

## Layout coordinate model

`layout()` returns local-space boxes (relative to parent content origin). `finalize_positions()` walks the tree and translates to absolute screen coords — it uses `translate_one()` (single node only), NOT `translate()` (recursive). Getting this wrong causes double-offset bugs.

## General

No memories. Anything that would go in a memory file goes in CLAUDE.md instead.

## Replay system

`cargo run --release -- --replay <script>` runs a scripted headless session and saves PNGs to `replay_screenshots/<name>/`. Available scripts: `close-tab`, `type-text`, `drag-select`, `drag-drop`.

`AppState::new_headless(canvas, gl_surface, gl_context, width, height)` constructs a full app state without a window, using the demo scene. All input handling lives in `impl AppState` methods (`on_key`, `on_mouse_press`, `on_mouse_drag`, `on_mouse_release`, `on_mouse_wheel`, `on_ime`, `do_render`, `capture_pixels`) — replay calls these directly. The replay system must never duplicate application logic; it is a virtual user, not a parallel implementation.

`ScriptedEvent::DragFrom` calls `on_mouse_press` + `on_mouse_drag` steps + `on_mouse_release`. `ClickAt`/`Click`/`ShiftClickAt` also call `on_mouse_release`. Every event that presses must release.

When writing replay scripts: read the first screenshot to find actual pixel coordinates before writing interaction events. Coordinates in comments are approximate and drift as layout changes.

## Debugging

Do not remove debug prints until the problem is confirmed fixed by actual testing. Compile does not mean correct.

When a bug is hard to repro interactively, add a replay script that exercises it and take screenshots before and after the operation under test. The screenshot is ground truth; the test passes when the image is correct, not when the code looks right.

## Style cascade

`compute_style()` resets non-inherited props to defaults, then applies matching rules sorted by specificity then source order. `NodeDesc.classes` is a `HashSet<String>`.
