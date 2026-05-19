# Arabic BiDi Support Plan

## Current state (see `replay_screenshots/japanese_text/0001_after_type.png`, line 13)

Line 13 shows `// Arabic: ٱلرَّحۡمَٰنِ ٱلرَّحِيمِ` rendered completely broken:
- Characters appear in logical (storage) order, not visual order — Arabic reads RTL but is painted LTR
- Characters are shaped as isolated forms, not joined (Arabic requires contextual joining: initial/medial/final/isolated)
- Diacritic marks (harakat) are positioned incorrectly relative to their base characters
- No proper ligature formation (e.g. lam-alef must merge into a single glyph)

This is because the current pipeline is char-by-char with zero text shaping:
`layout_text()` in `src/render/glyph_cache.rs` iterates `text.chars()`, maps each codepoint to a glyph ID via `charmap.map(ch)`, and records raw advance widths. No BiDi reordering, no Arabic joining, no ligature shaping. The same issue exists in `draw_text_femtovg()` in `src/render/paint.rs`.

---

## What is required for correct Arabic rendering

### 1. Unicode BiDi Algorithm (UBA, UAX #9)
Arabic text embedded in an LTR document requires the Unicode BiDi Algorithm. Given the mixed string
`// Arabic: ٱلرَّحۡمَٰنِ ٱلرَّحِيمِ`, the UBA must:
- Identify runs of LTR (ASCII comment prefix) and RTL (Arabic) characters
- Reverse the visual order of RTL runs relative to the LTR baseline
- Resolve embedded directionality correctly for mixed lines

### 2. OpenType Shaping (Arabic script)
Within an Arabic run, each character needs:
- Contextual form selection (GSUB: `init`, `medi`, `fina`, `isol` lookups)
- Ligature substitution (GSUB: `liga`, especially lam-alef)
- Mark positioning (GPOS: `mark`, `mkmk` — diacritics over/under base glyphs, with x/y offsets)
- Cursive attachment (GPOS: `curs`)

This is the job of a text shaping engine. Swash does not do OpenType shaping; `rustybuzz` does.

### 3. Cluster mapping
After shaping, the visual glyph sequence no longer corresponds 1:1 to codepoints. Multiple codepoints
may map to one glyph (ligature), and one codepoint may produce multiple glyphs (decomposition). The
`cluster` field on each shaped glyph records the byte offset back into the source string. All cursor
and hit-test logic must use this mapping instead of char-counting.

---

## Chosen dependencies

```toml
rustybuzz = "0.14"       # pure-Rust HarfBuzz port: BiDi + Arabic GSUB/GPOS shaping
unicode-bidi = "0.3"     # UAX #9 paragraph/run analysis
```

`unicode-bidi` handles paragraph-level analysis and run splitting.
`rustybuzz` shapes each run, producing per-glyph `(glyph_id, x_advance, x_offset, y_offset, cluster)`.

---

## Core new data structures (in `src/render/glyph_cache.rs`)

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BiDiDir { Ltr, Rtl }

pub struct ShapedGlyph {
    pub glyph_id: u16,
    pub x_advance: f32,   // pixels; converted from HarfBuzz design units via value * size_px / upem
    pub x_offset: f32,    // GPOS x correction (e.g. diacritic placement)
    pub y_offset: f32,    // GPOS y correction
    pub cluster: u32,     // byte offset into &text[run.text_range.clone()] (the run's substring)
}

pub struct ShapedRun {
    pub glyphs: Vec<ShapedGlyph>,   // in visual order within this run
    pub direction: BiDiDir,
    pub font_bytes: Option<Arc<Vec<u8>>>,  // None = primary font (use caller's font_data); Some = fallback
    pub font_face_index: usize,
    pub font_index: u8,             // GlyphKey discriminator (0=sans, 1=mono, 2+=fallback)
    pub text_range: Range<usize>,   // byte range in the original full line string
    pub total_advance: f32,         // sum of x_advance for all glyphs in the run
}
```

`ShapedLine` is `Vec<ShapedRun>` in visual left-to-right order. Each run's `text_range` maps glyphs
back to the original logical string for cursor math.

**Critical: `cluster` is a byte offset into the run's own substring (`&text[run.text_range.clone()]`),
not into the full line string.** This is because each sub-run is shaped with its own `UnicodeBuffer`
containing only the sub-run's text. To convert a `cluster` value to a byte offset in the full line
string, add `run.text_range.start`: `full_byte = run.text_range.start + cluster as usize`.

---

## New functions

### `shape_line()` — `src/render/glyph_cache.rs`

```rust
pub fn shape_line(
    cache: &mut GlyphCache,
    font_data: &'static [u8],
    font_index: u8,
    text: &str,
    size_px: f32,
    hint: bool,
) -> Vec<ShapedRun>
```

Algorithm:
1. `unicode_bidi::BidiInfo::new(text, None)` → paragraph info.
2. Get visual runs via `bidi_info.visual_runs(&bidi_info.paragraphs[0], 0..text.len())`.
   Each `VisualRun` has a byte range and a `Level` (even=LTR, odd=RTL).
3. For each visual run in visual order:
   a. Extract the substring `&text[run.range.clone()]`.
   b. Font selection: try primary font first. If any char in the run maps to glyph 0, fall back
      to OS fonts via `FallbackFontDb::find`. If the run contains mixed coverage, split it into
      sub-runs per font boundary (walk chars, detect font switches, emit a new sub-run when the
      font changes). Within each sub-run all characters use the same font.
      Each sub-run's `text_range` is its byte range within the full line string (a slice of the
      parent BiDi run's range).
      **RTL sub-run ordering**: when the parent BiDi run is RTL, the font-boundary sub-runs are
      identified in logical (left-to-right string) order, but must be emitted into the output
      `Vec<ShapedRun>` in visual order — i.e., reversed relative to logical order. Collect all
      sub-runs for an RTL parent run into a temporary Vec, then reverse it before appending to the
      output. (LTR parent runs: no reversal needed.)
   c. Build a `rustybuzz::UnicodeBuffer`, push the substring, set direction
      (`BufferDirection::RightToLeft` for odd levels), set script via Unicode block lookup.
   d. `rustybuzz::shape(face, features, buffer)` — default features are enough for Arabic
      (shaper auto-enables `init`, `medi`, `fina`, `liga`, `mark`, `mkmk`, `curs`).
   e. Collect `buffer.glyph_infos()` and `buffer.glyph_positions()`. Convert from HarfBuzz
      font units to pixels: `value as f32 * size_px / face.units_per_em() as f32`.
   f. Rasterize each glyph id into the atlas via `get_or_rasterize()` (same call as today,
      but with glyph_id from the shaper rather than from charmap lookup).
   g. Build `ShapedRun` with `text_range` = original byte offset of this visual run in the
      full `text` string.
4. Return `Vec<ShapedRun>` in visual order (LTR across the line).

### `measure_shaped_width()` — `src/render/glyph_cache.rs`

```rust
pub fn measure_shaped_width(
    font_data: &'static [u8],
    text: &str,
    size_px: f32,
) -> f32
```

Same algorithm as `shape_line()` but skips rasterization and atlas touches entirely — just sums
`x_advance` across all runs. Called from `measure_text_width()` (which is called from layout, without
a `GlyphCache`). This replaces the current char-by-char summing in `measure_text_width()`.

**Font selection in the measurement path.** `measure_shaped_width` cannot call `shape_line` directly
(that requires a `GlyphCache` for rasterization), but it must still perform the same font-coverage
detection to know which font to shape each sub-run with. Concretely: for each BiDi visual run,
check each character against the primary font's charmap; for chars that map to glyph 0, call
`fallback_db().lock()` and use `FallbackFontDb::find()` to discover the fallback font, then build
a `rustybuzz::Face` from the fallback bytes and shape just that sub-run. No `GlyphCache` is touched.
The `fallback_db()` mutex is already used in the current `measure_text_width()`, so this is not a
new lock dependency.

### `col_to_x_in_shaped_line()` — `src/render/glyph_cache.rs` (new, public)

```rust
pub fn col_to_x_in_shaped_line(runs: &[ShapedRun], col: usize, text: &str) -> f32
```

Converts a logical char offset `col` into a pixel x position within the shaped line.
- Walk the runs in visual order, accumulating `pen_x`.
- Convert `col` (char offset in the full string) to `target_byte`: the byte offset in the full
  string (`text.char_indices().nth(col).map(|(i,_)| i).unwrap_or(text.len())`).
- For each run: compute `run_target_byte = target_byte.wrapping_sub(run.text_range.start)` and
  check if `target_byte` falls within `run.text_range`. If not, skip (accumulate run's
  `total_advance` into `pen_x`).
- Within the matching run, walk glyphs in visual order. Each glyph's `cluster` is already a byte
  offset into the run's substring; compare against `run_target_byte`.
  - LTR run: return `pen_x` at the first glyph whose `cluster >= run_target_byte`.
  - RTL run: glyphs are in visual (reversed) order; the glyph whose cluster matches is to the
    right of the cursor's visual position. Return `pen_x + glyph.x_advance` for the glyph whose
    `cluster == run_target_byte`, or `pen_x` at the last glyph past the target.
- After ligatures, multiple chars share a cluster. Place the cursor at the ligature boundary
  (at its start for LTR, at its end for RTL). Splitting within a ligature is deferred.

**Word-wrap and `pen_x` origin.** `col_to_x_in_shaped_line` returns an x value relative to a
`pen_x` that starts at 0 and accumulates across the full shaped line — it has no knowledge of
visual row breaks. For **non-wrapping lines** (the common case in a code editor) this is fine:
the caller adds the span's `border_box.x` origin and gets the correct screen coordinate. For
**wrapped lines**, different visual rows have different `border_box.x` origins, so a flat pen_x
accumulation gives the wrong absolute x for runs on rows after the first. **Scope this function
to non-wrapping lines only.** The existing line wrapping (`visual_rows_for_line`) is driven by the
layout engine assigning different `border_box.y` values to spans; the shaped rendering path does
not need to replicate or extend that logic. If a line is long enough to wrap, cursor placement on
the wrapped portion will use the span's own `border_box.x` as the row origin, and
`col_to_x_in_shaped_line` should be called with only the runs belonging to that visual row (filtered
by `text_range` overlap with the row's spans), not the full line's runs. The callers in `main.rs`
are responsible for this filtering using the existing `visual_rows_for_line` / `char_base_for_row`
infrastructure.

### `x_to_col_in_shaped_line()` — `src/render/glyph_cache.rs` (new, public)

```rust
pub fn x_to_col_in_shaped_line(runs: &[ShapedRun], target_x: f32, text: &str) -> usize
```

Converts a pixel x coordinate into the closest logical char offset.
- Walk runs in visual order, tracking `pen_x` for each glyph.
- For each glyph, the clickable region is `[pen_x, pen_x + x_advance)`.
- When `target_x` falls in a glyph's region, decode its `cluster` (byte offset into the run's
  substring) back to a char offset in the full `text` string:
  `full_byte = run.text_range.start + cluster as usize`, then count chars before `full_byte`.
- **Rounding — LTR run**: click in the left half of the glyph → char at `cluster`; right half →
  next char (cluster of next glyph, or run end). "Left half" means `target_x < pen_x + x_advance/2`.
- **Rounding — RTL run**: the rounding direction is inverted because the glyph's visual left edge
  corresponds to the *end* of the logical character, not the start. Click in the right half
  (`target_x >= pen_x + x_advance/2`) → char at `cluster`; left half → next logical char.
  "Next logical char" for RTL means **lower `cluster` value** (the preceding glyph in the visual
  `glyphs` array, since HarfBuzz emits RTL glyphs in visual order so clusters decrease as `pen_x`
  increases). Concretely: in an RTL run, glyph[0] has the highest cluster value and is painted at
  the leftmost pen_x; glyph[last] has the lowest cluster value and is painted rightmost. "Previous
  glyph in visual order" (= glyph[i-1]) has a higher cluster value (earlier in the run substring
  visually, but later in logical byte order). So for RTL click-left-half rounding, the target
  logical char is the one at the cluster of glyph[i-1], or run end if i==0.
- For RTL runs, "char start" visually is the right side of the glyph; the mapping is still back
  to the logical char offset in the stored string.

---

## Changes to rendering — `src/render/paint.rs`

### `draw_text()` and `draw_text_femtovg()` — unified approach

Both rendering paths must be updated. The femtovg path (`draw_text_femtovg`, lines 34–103) currently
calls `canvas.fill_text()` which does its own shaping internally — that would conflict with our
shaping. The solution: **use the swash atlas path (`draw_text`, lines 105–154) for all text,
including text that currently uses the femtovg path.** The swash path already handles fallback fonts
correctly and produces correct glyph pixels. The femtovg path's only advantage was batching primary-font
runs into a single `fill_text` call; with shaping, that optimization is no longer safe anyway
(the shaper may reorder glyphs). Remove or bypass `draw_text_femtovg` entirely.

**Critical: set `use_femtovg = false` in `AppState::new_headless()`.** The headless constructor
currently sets `use_femtovg: true`, which routes all text through `draw_text_femtovg` and bypasses
the swash atlas path entirely. If this is not changed, replay/screenshot runs will silently use the
old unshaped path while the windowed app uses the shaped path — the two would behave differently and
the replay tests would prove nothing about shaping correctness. Change the field initializer in
`new_headless` to `use_femtovg: false` at the same time `draw_text_femtovg` is removed/bypassed.

**Paint pass: shape per line, not per span.** `draw_text()` is called once per span with just that span's text. Shaping per span would break Arabic joining across token boundaries. The solution is to move shaping up one level: in `paint_tree` (or the call site that iterates spans for a single editor line), collect the full concatenated text of all spans on the line, call `shape_line()` once, then iterate the resulting `Vec<ShapedRun>` and paint each glyph at its correct pen_x. Each run's `text_range` identifies which span(s) it overlaps, and the run's glyphs are painted starting at the span's `border_box.x` adjusted for the run's offset within the span.

Concretely, `draw_text()` as a per-span function is replaced by a per-line function:

```rust
fn draw_line(
    &mut self,
    line_lb: &LayoutBox,     // the line's layout box (children = spans)
    line_node: &Node,        // the line node (children = span nodes)
    font_size: f32,
    family: &str,
) {
    // Concatenate all span texts and their byte-range offsets.
    let mut full_text = String::new();
    let mut span_starts: Vec<usize> = Vec::new(); // byte offset of each span in full_text
    for span_node in line_node.children() {
        span_starts.push(full_text.len());
        full_text.push_str(span_text_of_node(span_node));
    }

    let (font_data, font_index) = self.font_data_for(family);
    let runs = shape_line(self.glyph_cache, font_data, font_index, &full_text, font_size, self.hint);

    // Determine the x origin for each run: the border_box.x of the span whose text_range
    // contains the run's start byte. Since runs stay within a single visual row in the
    // non-wrapping case, the run's pen_x origin is the span box that owns its first byte.
    let mut pen_x = 0.0_f32; // accumulated within the line; caller sets the line origin
    let line_origin_x = line_lb.border_box.x; // or content_box.x if padding applies

    for run in &runs {
        for g in &run.glyphs {
            let font_data_for_glyph = run.font_bytes.as_deref()
                .map(|b| b.as_slice())
                .unwrap_or(font_data);
            if let Some(cg) = self.glyph_cache.get_cached(g.glyph_id, run.font_index, font_size) {
                if cg.width > 0 && cg.height > 0 {
                    let gx = (line_origin_x + pen_x + g.x_offset + cg.bearing_x as f32).round();
                    let gy = (line_lb.border_box.y + font_size - g.y_offset - cg.bearing_y as f32).round();
                    // paint atlas rect at (gx, gy) — same atlas blit as before
                }
            }
            pen_x += g.x_advance;
        }
    }
}
```

The existing `paint_tree` span iteration loop is replaced with a call to `draw_line` at the line level. The per-span color (from syntax highlighting) is looked up by matching the glyph's `run.text_range` against `span_starts` to find which span owns that glyph, then reading that span node's `style.color`.

No per-frame shaping cache is needed. `shape_line` on a ~100-char editor line takes microseconds; shaping all visible lines (typically 40–60) per frame adds under a millisecond.

---

## Changes to measurement — `src/render/femtovg_measurer.rs` and `glyph_cache.rs`

`measure_text_width()` (line 359 of `glyph_cache.rs`) currently does char-by-char summing.
Replace its body with a call to `measure_shaped_width()`. Signature unchanged — callers (including
`SwashMeasurer::measure_width` in `femtovg_measurer.rs`) need no changes.

---

## Changes to cursor and interaction — `src/main.rs`

This is the most involved part. Six functions currently call `text_prefix_width()` and perform
char-counting, all of which break with shaped/BiDi text. Each must be updated.

### Central helper change: `text_prefix_width()` → `shaped_prefix_x()`

`text_prefix_width()` (line 1326) currently calls `measure_text_width(data, prefix, font_size)`.
With shaping, the "prefix of N chars" idea breaks for RTL text: visually, the first N logical chars
of `// Arabic: ٱلرَّحۡ...` may end up on the right side of the Arabic run, not the left.

The new primitive is: **given a shaped line (pre-computed `Vec<ShapedRun>`) and a logical col,
return the pixel x where the cursor should appear** — i.e., `col_to_x_in_shaped_line()`.

This means the six call sites stop calling `text_prefix_width` and instead call into a shaped
version. To avoid re-shaping every frame, the shaped result must be cached. See "Shaping cache"
below.

### No shaping cache needed

The cursor/paint functions are free functions, not `AppState` methods — plumbing a cache through all their signatures would be invasive and wrong. More importantly, it isn't needed: `rustybuzz::shape()` on a ~100-char editor line takes microseconds. The existing highlight cache rebuilds full syntax highlighting on every edit; shaping a few visible lines per frame is cheaper. Shape inline at each call site, no cache.

### Rewriting the cursor functions

The six cursor/hit-test functions are currently all built around span-walking and char-prefix
measuring. They need to be **rewritten** — not just have `text_prefix_width` swapped out — because
span boundaries are irrelevant to BiDi math. A shaped run (especially a ligature) can straddle span
boundaries, and the x↔col mapping must work across the full line text regardless of how spans divide
it. The new versions accept the full line's `Vec<ShapedRun>` and the full concatenated line text
string, and ignore the span tree entirely for the x↔col conversion.

The span tree is still used for one thing: determining the **visual row** for wrapped lines (from
`border_box.y`). That part doesn't change.

**Layout does not need to change.** Each span is still a left-to-right layout box; the box
positions are assigned by the existing LTR layout engine. What changes is only the
`x→col` and `col→x` math *within* a span's painted content, which is what the shaped runs handle.

### `cursor_visual_row_and_x()` (line 1448)

Rewrite: shape the full line text via `shape_line()`. Use `col_to_x_in_shaped_line(runs, col, line_text)` to get visual x. Determine visual row (for word-wrapped lines) by finding which span `border_box.y` contains the glyph at that col — do this by finding which run contains the col's byte offset, then checking that run's text_range against span byte ranges.

### `col_at_x_on_row()` (line 1479)

Rewrite: shape the full line text. For a given visual row, filter to the `ShapedRun`s whose `text_range` falls within that row's spans. Call `x_to_col_in_shaped_line(row_runs, target_x, line_text)`.

### `x_for_col_on_row()` (line 1615, used by `paint_selection`)

Rewrite: call `col_to_x_in_shaped_line(runs, col, line_text)` on the full line's shaped runs.

### `paint_cursors_with_text()` (line 1644)

Rewrite: shape the full line text, call `col_to_x_in_shaped_line(runs, col, line_text)` to get
`cursor_x`, draw 2px cursor bar at that x. The span-walk loop is removed entirely.

For RTL text the cursor bar appears at the visual "before" position of the logical character,
consistent with HarfBuzz cluster boundary semantics.

### `paint_drop_cursor()` (line 1698)

Same rewrite as `paint_cursors_with_text`.

### `hit_test_editor()` (line 1771)

1. Best-visual-row scan by `border_box.y` — unchanged.
2. Identify which logical line was hit — unchanged.
3. Replace the per-span char-prefix loop (lines 1850–1881) with: shape the full line text,
   call `x_to_col_in_shaped_line(runs, mx, line_text)`. This correctly handles RTL: clicking
   on the visual right side of an Arabic word maps to a *smaller* logical col (earlier in the
   string), not a larger one.

Note on ligatures: if `cluster` maps N input chars to 1 glyph (e.g. lam-alef), any click
anywhere within that glyph snaps to the same logical col (the cluster's start). This is the
only sensible behavior and is not a temporary limitation.

---

## Arrow key cursor movement

### Left/Right arrows (lines 487–531)

These currently do `col - 1` / `col + 1` in char space, which is always logical movement.
For a text editor, logical cursor movement (arrow keys move through the string byte-by-byte) is
the most predictable behavior and is what VS Code, Zed, and most editors do for mixed-direction
text. **No change needed to arrow key logic.**

The visual cursor position will automatically be correct once `cursor_visual_row_and_x()` is
fixed to use `col_to_x_in_shaped_line()`.

### Ctrl+Left / Ctrl+Right (word movement, lines 480–510)

`buf.word_start_left()` and `buf.word_end_right()` in `buffer.rs` work on the logical string.
No change needed — word boundaries in logical order are correct. The Arabic word `ٱلرَّحۡمَٰنِ`
will be treated as a single word because it contains no ASCII word-separator chars.

### Up/Down arrows (lines 532–563)

`move_cursor_vertical()` (line 1362) calls `col_at_x_on_row()` to find the col nearest to
`ideal_x` on the target row. Since `col_at_x_on_row()` is being fixed to use shaped layout,
up/down movement will automatically work correctly.

`cursor_ideal_x` is set once (at the start of a vertical movement chain) via `compute_cursor_x()`
→ `cursor_visual_row_and_x()`. Once that function is fixed, ideal_x will be the correct visual
pixel for mixed-direction text.

### Home / End (lines 564–579)

Home sets col=0 (start of logical line). End sets col=line char length. These are correct for
a code editor — Home/End navigate logical line boundaries. No change needed.

---

## Selection painting — `paint_selection()` (line 1531)

Currently: for each visual row in the selection range, calls `x_for_col_on_row()` to get
`x_left` and `x_right`, then paints a solid rectangle between them.

After shaping, this is no longer always a contiguous rectangle. Example:
`hello ٱلرَّحۡمَٰنِ world` — selecting from "hello" through the Arabic word would produce:
- A highlight rectangle covering "hello " (LTR, left side)
- A highlight rectangle covering the Arabic word (RTL, but visually in the middle)
- The LTR "world" at the right is not selected yet

The correct approach is to paint one highlight rectangle **per shaped run** that intersects the
selection range, rather than one rectangle per visual row:

```
for each ShapedRun in the line that overlaps [sel_start_col, sel_end_col):
    compute x_left = col_to_x_in_shaped_line for the run's start-within-selection
    compute x_right = col_to_x_in_shaped_line for the run's end-within-selection
    paint rectangle (x_left, row_y, x_right - x_left, line_h)
```

For RTL runs, x_left and x_right may be swapped relative to logical order; use `min`/`max` to
get the correct pixel rectangle.

Do not use the "single bounding box per row" MVP fallback. Even though it's less code, it draws
highlight over unselected pixels when an LTR and RTL run are interleaved on the same row. The
per-run rectangle approach is not significantly more code and is required for "real support."

---

## Span structure and text flow

The editor renders each logical line as a sequence of **spans** (one per syntax-highlighted token).
Each span is a separate layout box. `shape_line()` must operate on the **concatenated text of all
spans on a line**, not span-by-span, because Arabic joining can cross token boundaries.

Concretely: when collecting the shaped runs for a line, concatenate all span texts in order, run
`shape_line()` on the whole string, then re-associate each `ShapedRun` with its originating span
by byte-range overlap. The `text_range` field on `ShapedRun` makes this straightforward.

This is important: shaping a span like `ٱلرَّ` in isolation would give different joining forms
than shaping `ٱلرَّحۡمَٰنِ` as a whole. In practice, Arabic in code comments is always in one span
(the "comment" token), but the architecture should not assume this.

---

## Font selection for Arabic runs

`FallbackFontDb::find()` currently iterates all system fonts in arbitrary insertion order and
returns the first one with a matching glyph. For Arabic, the first match may be a font with
incomplete GSUB/GPOS tables (e.g. a CJK fallback that happens to include a few Arabic codepoints
without proper shaping data).

`rustybuzz::Face` is constructed from `&[u8]` (the raw font bytes already held in
`FallbackFontDb::bytes_cache`). This is a one-liner: `rustybuzz::Face::from_slice(bytes, face_index as u32)`.

After finding a candidate font, verify that the face has a non-empty GSUB table by checking
`face.table_with_tag(rustybuzz::ttf_parser::Tag::from_bytes(b"GSUB")).is_some()`. If not,
continue searching. This check only runs once per font face at discovery time (inside
`FallbackFontDb::find`), not on every shape call. On Windows, "Traditional Arabic",
"Arabic Typesetting", and "Segoe UI" all have proper GSUB/GPOS.

---

## Fix `get_or_rasterize` face index

`GlyphCache::get_or_rasterize` currently calls `FontRef::from_index(font_data, 0)` — hardcoded
index 0. This is wrong for font collection files (`.ttc`) where the desired face may be at index > 0.
The function already receives `font_index: u8` (the `GlyphKey` discriminator), but that is a cache
discriminator, not the face index within the font file.

Fix: add a `face_index: usize` parameter to `get_or_rasterize` and pass it through to
`FontRef::from_index(font_data, face_index)`. All call sites:
- Primary font: `face_index = 0` (OpenSans is a single-face file).
- Fallback fonts via `shape_line`: use `ShapedRun.font_face_index`.
- Fallback fonts via the old `layout_text` path (if kept): use `FallbackGlyph.face_index`.

This is a pre-existing latent bug; fixing it as part of the shaping work avoids leaving it for later.

---

## Handling zero-advance glyphs (combining diacritics)

Arabic harakat (diacritics like fatha, kasra, shadda) typically have `x_advance = 0` and non-zero
`y_offset`. The existing `round_to_e()` function currently passes zero-advance through unchanged —
this must be preserved. The GPOS `y_offset` from rustybuzz gives the vertical shift; apply it when
painting the glyph (`baseline - glyph.y_offset` since y-down coords).

---

## Files to change

| File | What changes |
|------|-------------|
| `Cargo.toml` | Add `rustybuzz = "0.14"`, `unicode-bidi = "0.3"` |
| `src/render/glyph_cache.rs` | Add `ShapedGlyph`, `ShapedRun`, `BiDiDir`; add `shape_line()`, `measure_shaped_width()`, `col_to_x_in_shaped_line()`, `x_to_col_in_shaped_line()`; update `measure_text_width()` to call `measure_shaped_width()`; add `face_index: usize` param to `get_or_rasterize()` and fix hardcoded `0` |
| `src/render/paint.rs` | Replace `draw_text()` with `shape_line()` walk; remove or bypass `draw_text_femtovg()` |
| `src/render/femtovg_measurer.rs` | No signature change; body already delegates to `measure_text_width()` which is updated |
| `src/main.rs` | Update `cursor_visual_row_and_x()`, `col_at_x_on_row()`, `x_for_col_on_row()`, `paint_cursors_with_text()`, `paint_drop_cursor()`, `hit_test_editor()` to use `col_to_x_in_shaped_line()` / `x_to_col_in_shaped_line()`; update `paint_selection()` for per-run rectangles; update `japanese-text` replay script; set `use_femtovg: false` in `new_headless()` |

---

## What NOT to do

- Do not implement UAX #9 by hand. Use the `unicode-bidi` crate.
- Do not attempt Arabic shaping with Swash alone — Swash is a rasterizer/metrics crate.
- Do not change buffer storage order. Text is always stored in logical (Unicode) order; BiDi is display-only.
- Do not shape individual spans — shape the full concatenated line text to get correct joining across token boundaries.
- Do not conflate "logical left arrow" with "visual left arrow". Keep arrow keys as logical movement (col±1). Implement visual cursor movement only if explicitly requested later.
- Do not try to split selection rectangles and cursor/arrow behavior in the same step. Order of work: (1) shaping + rendering, (2) measurement + layout, (3) cursor painting, (4) hit testing, (5) selection painting.

---

## Testing

### Render test
Run `cargo run --release -- --replay japanese-text` and inspect
`replay_screenshots/japanese_text/0001_after_type.png`.

Line 13 `// Arabic: ٱلرَّحۡمَٰنِ ٱلرَّحِيمِ` should show:
- `// Arabic: ` prefix in normal LTR order (unchanged)
- Arabic runs in RTL visual order: the *second* word `ٱلرَّحِيمِ` appears to the left of the first `ٱلرَّحۡمَٰنِ`, with characters joined (cursive), diacritics stacked above/below base glyphs, lam-alef ligatures formed

### Cursor test (add to the replay script)
```rust
ClickAt(x_inside_arabic_word, y_of_arabic_line),
ScreenshotNamed("arabic_cursor"),
// Cursor bar should appear inside the Arabic word at the clicked glyph boundary,
// not at the LTR end of the line.
ShiftClickAt(x_different_arabic, y_of_arabic_line),
ScreenshotNamed("arabic_selection"),
// Selection highlight should cover the clicked range within the Arabic run.
```

### Hit-test regression test
After clicking on the Arabic word, arrow-key left/right should move the cursor within the word
(col changes ±1 in logical order). Screenshot each step to verify cursor bar moves through the
word consistently.

### Existing tests
`cargo test` — all 60 existing tests must still pass. The test suite does not exercise shaping,
so no test changes are needed for the shaping itself, but `measure_text_width` is exercised
indirectly by layout tests; verify these still pass after the measurement function is updated.
