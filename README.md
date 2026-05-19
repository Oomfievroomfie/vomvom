*This README was written by Claude (claude-sonnet-4-6) based on a skim of the source.*

---

# vomvom

A text editor entirely vibecoded in Rust. Early stage.

<img width="589" height="556" alt="2026-05-19_02-18-09" src="https://github.com/user-attachments/assets/e861ec03-7e47-4061-b271-d935b38f0910" />

The UI uses a custom HTML/CSS-alike layout engine: parse `.htmv` markup, apply a cascade from a `.cssv` stylesheet (selectors, specificity, inheritance), lay out nodes in block/flex/inline, then paint with femtovg. The toolbar and menus are `.htmv` files compiled in with `include_str!`.

Text is shaped with `swash` and `rustybuzz` (for Arabic/BiDi), rasterized to an RGBA atlas, and composited as premultiplied white-with-alpha quads. There are two rendering paths (swash direct and femtovg's own text path) that can be toggled at runtime.

Session persistence is SQLite in WAL mode. Every buffer, its undo stack, scroll position, and the active tab are synced every 5 minutes and restored on next launch — no "restore session?" prompt. Undo ops are stored individually with a group ID for coalescing, so undo/redo survives a crash.

The layout engine has a notable gotcha documented in the source: `layout()` returns local-space coordinates (relative to parent content origin), and a separate `finalize_positions()` pass converts to absolute screen coords using `translate_one()` on each node — not the recursive `translate()`, because that would double-count offsets.

Scripted headless replay is available for debugging: write a script of events (`ClickAt`, `DragFrom`, `Type`, etc.), run `cargo run --release -- --replay <name>`, and get PNGs written to `replay_screenshots/`. This exists because some bugs are hard to reproduce interactively.

## Building

```
cargo run --release
```

For a headless screenshot:

```
cargo run --release -- --screenshot
```

Fonts (`OpenSans-Medium.ttf`, `Sono-Medium.ttf`) are embedded at compile time from the project root.

## Status

This is a personal project. It opens files, edits them, highlights Rust syntax, and handles Arabic text. It is not ready for anyone to use as their daily editor.
