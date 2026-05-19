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

## Tool use rules

- Never use the Explore subagent for searches that cover a small number of files. Only use it for very wide-cast searches across a large codebase.
- Never use tail/find/grep/head/sed in complex Bash invocations to read files. Always use the dedicated Read, Glob, and Grep tools instead.

## Replay system

`cargo run --release -- --replay <script>` runs a scripted headless session and saves PNGs to `replay_screenshots/<name>/`. Available scripts: `close-tab`, `type-text`, `drag-select`, `drag-drop`.

`AppState::new_headless(canvas, gl_surface, gl_context, width, height)` constructs a full app state without a window, using the demo scene. All input handling lives in `impl AppState` methods (`on_key`, `on_mouse_press`, `on_mouse_drag`, `on_mouse_release`, `on_mouse_wheel`, `on_ime`, `do_render`, `capture_pixels`) — replay calls these directly. The replay system must never duplicate application logic; it is a virtual user, not a parallel implementation.

`ScriptedEvent::DragFrom` calls `on_mouse_press` + `on_mouse_drag` steps + `on_mouse_release`. `ClickAt`/`Click`/`ShiftClickAt` also call `on_mouse_release`. Every event that presses must release.

### Writing a new replay script

1. Add a match arm in `run_replay_script` in `src/main.rs`, call `replay::run_script("folder_name", 1024, 768, vec![...])`, and add the script name to the available list in the catch-all error message.
2. Start every script with `ScreenshotNamed("initial")` so you can see the starting state and read pixel coordinates from it before writing any interaction events. Coordinates in comments drift as layout changes — always verify from the actual screenshot.
3. The demo buffer (active tab) starts with 8 lines of Rust code. `Type("text")` inserts at the cursor (end of buffer by default). Typed text appears immediately on the next screenshot.
4. For mouse interactions, use `ClickAt(x, y)` for single clicks, `DragFrom(x1, y1, x2, y2)` for click-and-drag. Read the initial screenshot to find real coordinates — toolbar is ~28px tall, tab bar ~28px, editor content starts around y=60.
5. Take screenshots before and after each operation you're debugging. Name them descriptively with `ScreenshotNamed("label")`.
6. Run with `cargo run --release -- --replay <script-name>` and read the output PNGs from `replay_screenshots/<folder_name>/`.

Available events: `Type`, `Backspace`, `Delete`, `MoveCursor(line, col)`, `MouseMove`, `Click`, `ClickAt`, `ShiftClickAt`, `DragFrom`, `Undo`, `Redo`, `OpenMenu`, `CloseMenus`, `MenuAction`, `ScrollTo`, `Screenshot`, `ScreenshotNamed`.

## Debugging

Do not remove debug prints until the problem is confirmed fixed by actual testing. Compile does not mean correct.

When a bug is hard to repro interactively, add a replay script that exercises it and take screenshots before and after the operation under test. The screenshot is ground truth; the test passes when the image is correct, not when the code looks right.

## Known issues / TODO

- IME preedit preview is drawn as an overlay at the cursor position (opaque background, underline) but doesn't take up inline space — text after the cursor doesn't shift right. To fix properly: splice the preedit into the highlight token list for the cursor line before building the line node in `update_editor_node`, splitting the token at cursor.col. Strip it on `Ime::Commit` / empty preedit. The preedit must not enter the undo stack or session sync.

## Style cascade

`compute_style()` resets non-inherited props to defaults, then applies matching rules sorted by specificity then source order. `NodeDesc.classes` is a `HashSet<String>`.
