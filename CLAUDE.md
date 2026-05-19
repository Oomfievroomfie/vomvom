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
- `src/main.rs` — winit event loop + demo scene
- `src/screenshot.rs` — headless render path

## Layout coordinate model

`layout()` returns local-space boxes (relative to parent content origin). `finalize_positions()` walks the tree and translates to absolute screen coords — it uses `translate_one()` (single node only), NOT `translate()` (recursive). Getting this wrong causes double-offset bugs.

## General

No memories. Anything that would go in a memory file goes in CLAUDE.md instead.

## Debugging

Do not remove debug prints until the problem is confirmed fixed by actual testing. Compile does not mean correct.

## Style cascade

`compute_style()` resets non-inherited props to defaults, then applies matching rules sorted by specificity then source order. `NodeDesc.classes` is a `HashSet<String>`.
