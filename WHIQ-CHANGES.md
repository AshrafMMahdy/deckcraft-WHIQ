# WHIQ fork of DeckCraft — changes

Branch `whiq`. The White Hat IQ platform runs `deckcraft-cli mcp` headless, one process per caller
and document, behind a supervisor, with the read/write roots set to the chat's folder and the
author set to the caller. The agents are LLM agents in long unattended runs, so the owner's rule is
**no dead stops**: every limit is a recoverable answer, never a failure. This file lists every
change from upstream, why, and how it is tested. Upstream code was not reformatted; new work lives
in new files where it could.

Run everything: `cargo test` (workspace default members) — the contract suite is
`cargo test -p deckcraft-cli --test mcp_contract --test mcp_deck`. End-to-end smoke of the release
binary: `cargo build --release -p deckcraft-cli && python3 tests/mcp-contract/smoke.py`.

Build note: the binary embeds the Arabic fallback font (Noto Sans Arabic) from a
[craft-fonts](https://github.com/storytold/craft-fonts) checkout next to this repository
(`../craft-fonts`, see `.cargo/config.toml`); release builds should set `CRAFT_FONTS_REQUIRED=1`.

## 1. The shared "no dead stops" contract (MCP server)

| Contract item | What changed | Where | Test (`tests/mcp-contract/contract.rs`) |
|---|---|---|---|
| §1 errors | Every tool error is `structuredContent.error = {code, reason, next_step, …}` (`busy`, `evicted`, `too_large`, `not_found`, `invalid_argument`, `out_of_range`, `forbidden`, `no_document`, `unsupported`, `internal`, `failed`). `not_found` carries `candidates`: closest tool names, command ids, the shapes on the slide, similar file names. The text block reads `[code] reason` / `next_step: …`. | `crates/mcp/src/contract.rs`, `server.rs` | `errors_have_a_code_a_reason_and_a_next_step` |
| §2 nothing hangs | Calls run on one worker thread in order; the main thread keeps reading. A call that waited `--busy-after-ms` (8000) behind another is answered `busy` with `retry_after_ms`. `ping`, `tools/list`, `server_status`, `job_status`, `list_jobs` never wait. | `crates/mcp/src/runtime.rs` | `busy_comes_with_a_retry_time_and_status_answers_at_once` |
| §2 jobs | Opening a file of `--open-job-mb` (24) or more, and exporting every slide (PNG/JPEG `all`, PDF, GIF) of a deck of `--export-job-slides` (24) or more, run on a job thread (`async: true` forces it). The call returns `job_id` at once; `job_status` gives state, progress (per slide for images), `retry_after_ms` and the result, which stays in the job table (200 most recent) however late the client asks. Document tools answer `busy` (with the `job_id`) while a presentation is still opening. Exports work on a snapshot, so editing can go on. | `crates/mcp/src/jobs.rs`, `state.rs` | `long_work_runs_as_a_job` |
| §3 work is never lost | Unsaved work (dirty, or never saved) is written to `<name>.recovered-<YYYYMMDD-HHMMSS>.<ext>` in the write root (else the file's own folder, else the temp folder) on: idle eviction (`--idle-evict-secs`), memory pressure (`--max-memory-mb`, resident memory), the `evict` tool (for a supervisor), an internal failure (panic caught per call; the document stays open), SIGTERM / SIGINT / SIGHUP and Ctrl+C / Ctrl+Break / console close on Windows (`ctrlc` crate; if the worker is stuck the last snapshot is saved), the end of input, and `close_presentation`. The next answer names the file (`recovered_files`); a call on an evicted document answers `evicted` with "your work is auto-saved at X, call open_presentation on it". At start-up the write root is scanned for recovered files; `initialize` and `server_status` list them. Never deletes: a document that cannot be saved is not evicted. | `crates/mcp/src/autosave.rs`, `server.rs`, `runtime.rs` | `eviction_auto_saves_and_the_next_call_names_the_file`, `sigterm_saves_unsaved_work_and_the_next_run_names_it`, `an_internal_failure_saves_the_work_and_the_server_goes_on`, `end_of_input_keeps_unsaved_work` |
| §4 oversized results | Answers over `--max-result-bytes` (256 KiB) are cut at the largest array with `truncated: true`, `continue_from`, `total`, `truncated_path`, `next_step`; very long strings are shortened. `list_commands`, `inspect_document` (slides), `inspect_slide` (shapes), `batch`, `list_jobs` take `offset` / `limit`. `render_slide` images over 6 MiB are re-rendered at a smaller scale with a warning (`path` writes a full-size PNG instead). | `contract.rs` (`trim`, `page`), `server.rs` | `oversized_results_are_trimmed_and_continue` |
| §4 server_status | New read-only tool: version, protocol, pid, uptime, author, resident memory and limit, roots, open documents (title, path, slides, dirty), jobs, the call running now, idle time, busy count, recovered files, evicted documents, limits. Answers from shared state, never waits. | `server.rs` (`status_value`) | `oversized_results_are_trimmed_and_continue`, `busy_…` |
| §5 forgiving inputs | Tool names in any spelling (`renderSlide`, `Render-Slide`, `open`→`open_presentation`); argument names case- and separator-insensitive (`keep_going`→`keepGoing`, `file`→`path`); numbers and booleans given as strings coerced; command ids forgiving (`Slide.New`, `slide_new`). Unknown arguments are **warnings** on a successful call (upstream refused them; `crates/mcp/src/tests.rs` updated accordingly). | `contract.rs` (`resolve_tool`, `resolve_command`, `normalize_args`) | `inputs_are_forgiving`; unit tests in `contract.rs` |
| §6 paths, atomic saves, OLD_ | Upstream confinement kept (`--automation-read-root/--automation-write-root`, links followed, symlink escapes refused). Relative paths now resolve inside the write root (the server's working folder). Saves stay temp + fsync + rename, now also fsync of the folder; an existing file is never replaced silently: it is renamed `OLD_<name>` (`OLD_2_<name>`… if taken) first and the answer lists `previous_files_renamed` and a `notice`; `overwrite: true` replaces. Applies to every write a call makes (save, export, PNG sequences, PDF, build_deck, recovered files). Answers report absolute paths. | `crates/engine/src/io.rs` (`keep_previous`, `old_name_for`), `apps/deckcraft-cli/src/main.rs` | `saves_keep_the_previous_file_as_old`, `paths_are_confined_to_the_roots`, upstream `mcp_roots.rs` |
| §7 caller identity | `--author NAME` at start; every tool takes `caller`. Comments are authored by it; saving sets `lastModifiedBy` (and `creator` when empty) in `docProps/core.xml`; `build_deck` sets both. | `server.rs`, `tools.rs` | `the_caller_is_named_on_comments_and_files` |
| §8 annotations | `readOnlyHint` only on reads (`list_commands`, `inspect_*`, `render_slide`, `screenshot`, `server_status`, `job_status`, `list_jobs`); `destructiveHint` on tools that can delete or overwrite (`run_command`, `batch`, `call_app`, `key`, `pointer`, `type_text`, `save_presentation`, `export`, `build_deck`, `set_text`, `adopt_template`, `close_presentation`); `idempotentHint` on reads, `go_to_slide`, `select`, `select_tool`, `set_text`, `adopt_template`. `structuredContent` on every result (an object; arrays come as `{result: […]}`). | `crates/mcp/src/tools.rs` | `annotations_tell_reads_from_writes` (and every answer in the suite is checked by `common::Mcp`) |
| §10 bidi | A paragraph with no direction of its own takes its base direction from its first strong character (UAX #9 P2/P3): Arabic is RTL and right-aligned unless the paragraph aligns itself — in layout (`crates/text`) and in the saved file (`a:pPr rtl="1" algn="r"`, `crates/pptx` writer), so PowerPoint shows it the same. Arabic shaping and the Arabic fallback font come from upstream (harfrust, Noto Sans Arabic via craft-fonts); the vector PDF keeps Arabic as shaped, real text (§6 below). | `crates/text/src/lib.rs` (`shape_para`), `crates/pptx/src/write/dml.rs` (`paragraph`) | `arabic_paragraphs_are_right_to_left_by_default` |

Other server changes: `close_presentation` (auto-saves unsaved work first) and `evict` tools;
`Backend` gained `session()` / `automation_roots()` (default `None`) and is `Send`;
`Server::serve` now takes ownership and runs the concurrent runtime; `Server::handle_line` is
unchanged for in-process use. New CLI flags: `--author`, `--busy-after-ms`, `--idle-evict-secs`,
`--max-memory-mb`, `--max-result-bytes`, `--open-job-mb`, `--export-job-slides` (`deckcraft-cli
help mcp`). Test hooks (`_delay_ms`, `_panic` arguments) work only with
`DECKCRAFT_MCP_TEST_HOOKS=1`. New dependencies of `deckcraft-mcp`: `chrono` (workspace),
`memory-stats`, `ctrlc` (MIT/Apache-2.0).

## 2. build_deck — the platform's nine layouts (engine command `deck.build`, MCP tool `build_deck`)

A whole deck from one JSON spec, reproducing the shapes of the platform's own writer
(`shared/chat-engine/server/tools/write_doc_tier2.ts`, `renderPptx`) on its 13.333 × 7.5 in grid,
scaled to the deck's slide size: the title slide (band background, accent rule, subtitle), the
master decoration (bottom accent rule, optional logo, slide number field on every slide), the
heading with its accent rule, and the layouts:

| Layout | Spec keys | Drawn as |
|---|---|---|
| `bullets` (default) | `paragraphs`, `bullets` (strings with leading-space levels, or `{text, level}`), `quote` | paragraphs then bullets, quotes muted italic |
| `two-column` | `left` / `right` (lists, or `{heading, bullets}`), else `bullets` split in half | two text columns |
| `table` | `table` (`[[…]]`, `{header, rows}`), `rowsPerSlide?`, `fontSize?` | header-shaded, zebra rows, proportional column widths, **auto-paged** over as many slides as the rows need (by row height, or `rowsPerSlide`), header repeated, titles `Title (k/n)` |
| `callout` | `text`, `bullets` | accent2 band with white text, bullets below |
| `kpi` | `kpis: [{label, value}]` or `Label: Value` bullets (2–4) | cards, value size by length |
| `section` | `title`, `subtitle?` | band-coloured divider on the Section Header layout |
| `quote` | `quote`, `attribution` | accent2 band, italic, `— attribution` |
| `chart` | `chart: {kind: bar\|column\|line\|pie\|…, categories, series}` or a numeric `table` | native chart in the theme palette, legend when several series, data labels except line |
| `image` | `image` / `images`: `{path, position: full\|left\|right\|logo, caption}`, `overlays` | full (up to two side by side), half-slide left/right with the body narrowed beside it, logo top right |

Every slide takes `notes` (speaker notes). Inline `**bold**`, `*italic*`, `` `code` `` become
runs (no literal markers). Layout names are forgiving (`Two Columns`, `KPIs`, `divider`…); an
unknown one draws as bullets with a warning, like the platform. Titles go into the layouts' title
placeholders (so outline export and inspect see them). `theme`: `corporate` (default), `slate`,
`warm`, `mono` (the platform's colours; open font families by default per AGENTS.md §1 — pass
`fontHead` / `fontBody` to name others) or a palette object; the palette also goes into the master's
theme. `path` saves (.pptx, .potx, .deckcraft) with the OLD_ rule. Everything missing or unreadable
(a picture, a non-numeric chart table) is a `warnings` entry, never a failure.

Files: `crates/engine/src/cmd/build.rs` (new), `crates/engine/src/cmd/mod.rs` (registration),
`crates/mcp/src/tools.rs`. Tests: `tests/mcp-contract/deck.rs`
(`the_nine_layouts_each_with_speaker_notes`, `a_long_table_pages_over_slides_with_its_header`),
unit tests in `build.rs`. `deck.build` is skipped by the one-document soak test, like `file.new`.

## 3. The overlay vocabulary on pictures (`overlays` in build_deck, tool `add_overlays` / command `picture.overlay`)

The platform's markup on a picture as native, editable shapes, positioned in percent of the
picture box (`relativeTo: image` measures against the visible picture of a letterboxed one):
`rect`, `ellipse` (filled at 35 % opacity, or an outline when `dashed`), `line` (w/h are the end
point; right-to-left and upward legs are a positive box flipped, never a negative extent),
`path` (freeform through `points` `"5,50 12,48"` or `[[x,y],…]`, open as a route or closed and
filled with `filled`), `text` labels; `dashed`, `arrow: end|both` on lines and paths; labels centred
in boxes and above paths, as on the platform. Objects or the platform's overlay table rows
(`[shape, x, y, w, h, colour, label, points?, style?]`, with its `shape` header placing the
columns) are both accepted. `add_overlays` works on a picture already on a slide (in place, one undo
step). Tests: `overlay_regression_deck_renders_every_mark` (renders the regression deck to PNG —
kept as `target/tmp/overlay-regression.png` and `.pptx` — and probes each mark's pixels and the
DrawingML: `prstDash`, `headEnd`/`tailEnd`, `custGeom`, `close`, alpha),
`overlays_go_on_an_existing_picture_from_the_platform_table`.

## 4. Theme adoption from a company template, `.potx`

- `build_deck {template: company.pptx|.potx}` builds on the template itself: its theme (colours,
  fonts, effects), slide masters (logo and decorations included) and layouts; its own slides and
  the pictures only they used are dropped. Titles fill the template layouts' own title
  placeholders (position and look from the template); the title slide uses its Title layout,
  sections its Section Header layout; content is drawn in the template's palette lifted the way
  the platform's `deckTheme.ts` does (lt1 background, dk1 text, dk2 headings, accent1/2, forced
  contrast, major/minor fonts).
- `adopt_template {path}` (command `design.adoptTemplate`) restyles an open deck: the template's
  masters and layouts replace the deck's (ids moved past the deck's, master media copied and
  remapped), each slide moves to the template layout of the same kind (else the same name).
- `.potx` save writes `application/vnd.openxmlformats-officedocument.presentationml.template.main+xml`
  (upstream writer, now exercised by the tests).
- Test: `a_deck_built_from_a_company_template_takes_its_theme_masters_and_layouts` (template made
  with a custom palette and fonts, a logo on its master and a renamed layout; checks the content
  type, theme XML, master picture, layout names and that slides sit on the template's layouts).

## 5. Export and lossless in-place editing

- Export (upstream, now covered by the contract suite): slide PNG at any scale (`scale: 0.5` →
  480 × 270), every slide (as a job for big decks), PDF (slides / notes / handouts; vector artwork
  with real text since round 2, see §6), outline text, GIF. Test:
  `export_png_at_a_scale_pdf_and_outline`.
- **Unmodelled parts survive an open → edit → save.** Upstream kept only custom XML, sensitivity
  labels and custom properties. The reader now also keeps every part the presentation or a slide
  links with a relationship it does not model (add-in data, printer settings, revision info,
  extension parts…), with everything those parts link in turn, as text or bytes, with their
  content types and original relationship ids; the writer links them again (kept ids when free —
  relationship ids are now always unique, `RelsOut::fresh_id`) and never over a part it wrote.
  Macros (`vbaProject`) are left out: a .pptx cannot carry them. Media, animations, embedded fonts,
  slide extension lists, opaque objects (SmartArt, OLE) were already modelled upstream; the test
  proves them. Files: `crates/pptx/src/read/mod.rs` (`foreign_parts`, `PRES_KNOWN_RELS`,
  `SLIDE_KNOWN_RELS`), `crates/pptx/src/write/mod.rs` (`write_kept`, `kept_name_ok`),
  `crates/pptx/src/opc.rs` (`add_with_id`, `has`), `crates/model/src/lib.rs` (`Slide.kept_parts`,
  `KeptPart.bytes`, `KeptPart.rel_id`). Test:
  `editing_in_place_keeps_media_animations_fonts_and_unknown_parts` (a customer copy with an
  embedded font, deck- and slide-level add-in parts and a slide extension; every part name kept,
  media/font/add-in bytes identical, the fade entrance and the extension still in the slide XML,
  relationships and content types back).

## 6. Vector PDF (round 2)

Slides are written as vector artwork with text as text, in every page layout (slides, notes pages,
handouts); `raster: true` (export / `file.export`) keeps the upstream picture-per-slide export with
its invisible text layer. How: the renderer now draws on a `Canvas` trait (`crates/render/src/canvas.rs`)
instead of `vello_cpu::RenderContext` directly — `RenderContext` implements it unchanged for pixels,
and a `Recorder` keeps the drawing as a display list (fills, strokes, layers, text runs).
`crates/pdf/src/vector.rs` writes the list: paths with solid colours and linear/radial/sweep
gradients (spread modes, paint transforms), strokes (width, caps, joins, dashes), clip and opacity
layers, pictures as embedded images at the export `dpi` (tiled and mirrored fills included), and
text runs as glyphs of the embedded font subset with a ToUnicode map built from the run's logical
characters (search, select, copy; Arabic stays shaped and joined, marks placed by glyph offsets).
What PDF cannot express — blur filters (shadows, glows, soft edges), masks, non-normal blend modes,
picture-painted strokes — is painted to pixels for just that layer and placed as an image under the
vector artwork. Links (URLs and slide jumps) and bookmarks are kept as before. The recorded list
replayed onto pixels matches the renderer (test), so nothing is drawn differently.
Tests: `crates/pdf/src/tests.rs` (`slides_are_vector_artwork_with_real_text_unless_raster`,
`a_recorded_slide_replays_like_the_rendered_one`, the upstream link/bookmark/notes/handout tests),
`tests/mcp-contract/deck.rs` (`export_png_at_a_scale_pdf_and_outline`: embedded fonts, no slide
pictures, `raster: true` still a picture per slide), the smoke script (pdftotext finds the text).
A 13-slide built deck: 37 KB vector vs ~1 MB raster.

## Not done / known limits

- A relationship id kept from a file that collides with one the writer made is renamed, and kept
  XML that pointed at the old id is not rewritten (rare: writer ids are `rIdN`).
- A part linked both from the presentation and from a slide is written once and linked from the
  slide only.
- Busy is per process: one worker serves calls in order (one process per caller and document is
  the platform's model); a call cannot be preempted once it runs — jobs cover the long ones.

## Test status (Linux x86_64 container, 2026-10-17)

`cargo test` over every crate plus `deckcraft-cli`: 1635 passed, 13 failed, 6 ignored. The 13
failures are all in `deckcraft-ui-egui` (egui_kittest: "No adapter found" — no GPU in the
container; `replacefonts_tests::ribbon_button_opens_the_dialog_and_replaces` fails the same way on
upstream `2cf4535`). The desktop app `apps/deckcraft` does not build here (no ALSA headers for
`alsa-sys`). Everything the headless binary uses is green, including the 21 contract and deck
tests and the smoke script.
