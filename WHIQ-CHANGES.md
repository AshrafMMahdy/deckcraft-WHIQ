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

Build note: the binary embeds its fonts (Latin presentation faces with real italics, the Arabic
fallback, Japanese) from a [craft-fonts](https://github.com/storytold/craft-fonts) checkout; release
builds set `CRAFT_FONTS_DIR` and `CRAFT_FONTS_REQUIRED=1` — recipe in §9.

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

## 7. Cancelling a running call or a job (round 2)

New tool `cancel` (answered by the reader thread, never waits): it stops the call running now at
its next safe point, or the job `job_id`; `queued: true` also drops the calls waiting behind it
(each answered `cancelled`). Every call runs inside a `deckcraft_engine::task::Task`
(`crates/engine/src/task.rs`): progress and a cancel flag shared with the host. Safe points: between
the slides `deck.build` makes (nothing is added to the session until the whole deck is built),
between the slides a .pptx is read (`deckcraft_pptx::import_with`), between slides of an image export
(the files written so far are named), between PDF pages (`PdfOptions::on_page`), between GIF frames,
between the commands of a `batch`. A cancelled command fails with `cancelled` and the session puts
the presentation back as it was before the command (the engine's rollback of failed commands), so
it is always consistent. A cancelled open job never adds its file; a cancelled export job reports
what it wrote. New error code `cancelled`, job state `cancelled`.
Tests: `tests/mcp-contract/contract.rs` (`a_running_call_stops_at_a_safe_point_when_cancelled`:
cancel answers at once, the call stops long before its end, queued calls dropped, a job cancelled
leaves no document), `crates/engine/src/tests_task.rs` (a cancelled build adds no deck; cancelled
PDF, PNG and GIF exports write nothing; a cancelled read adds no document; PDF progress per page).

## 8. Relationship ids of kept parts (round 2)

- Kept parts are linked **first** in their holder's relationships (the presentation's, each
  slide's), under their original ids, so the writer's own relationships (layout, notes, pictures…)
  take other ids and can never take theirs. Should an original id be taken all the same, the part is
  linked under a new id past every kept one, and every kept XML of that holder that names the old id
  (the slide's or the presentation's extension list) is renumbered with it — relationship-id
  attributes only, all in one pass (no chains). `crates/pptx/src/write/mod.rs` (`link_kept`,
  `renumber_refs`).
- Extension lists that point at kept parts are now kept too (upstream dropped every extension list
  holding a relationship id); one pointing at anything else is still left out.
- A part linked from both the presentation and a slide is written once and linked from both.
- Tests: `kept_id_tests::a_taken_kept_id_is_renumbered_with_the_xml_that_names_it` (pptx unit
  test), `editing_in_place_keeps_media_animations_fonts_and_unknown_parts` (an add-in part on `rId1`
  — an id the writer would give the layout — named by a slide extension, and a part shared by the
  deck and a slide).

## 9. Fonts: the craft-fonts build input (round 2)

Same mechanism as wordcraft: `crates/fonts/build.rs` embeds the faces listed in a craft-fonts
checkout's `fonts/latin-manifest.txt` (presentation fonts) and `fonts/manifest.txt` (scripts) into
the binary, read from `CRAFT_FONTS_DIR` (local builds default to `../craft-fonts` through
`.cargo/config.toml`). With `CRAFT_FONTS_REQUIRED` set the build **fails** (native targets) when:

- `CRAFT_FONTS_DIR` is unset, or the checkout or a listed font file cannot be read;
- no embedded face covers Arabic (`Arab` — Noto Sans Arabic, the fallback every Arabic run without
  a font of its own is drawn with, so every machine draws it the same), Japanese (`Jpan`) or Latin;
- any of Inter (the default face), Carlito, Caladea, Liberation Sans/Serif/Mono (the
  metric-compatible stand-ins for Calibri, Cambria, Arial, Times New Roman, Courier New) has no
  real italic face — italics are then drawn with the font's own italic, not a slanted Regular.

`Hans` is not required (unlike wordcraft): craft-fonts' only Simplified Chinese face is Noto Sans
CJK SC, which DeckCraft's asset policy excludes (AGENTS.md §1, with Source Sans/Serif/Han); Chinese
text uses the Japanese faces' Han glyphs or an installed face.

Release recipe:

```sh
git clone https://github.com/storytold/craft-fonts ../craft-fonts
git -C ../craft-fonts checkout 43913056ce82a3fdad0ef69f6b88d1dc922a8104   # the pin the three craft forks share (whiq-release.yml)
CRAFT_FONTS_DIR="$PWD/../craft-fonts" CRAFT_FONTS_REQUIRED=1 cargo build --release -p deckcraft-cli
```

(Windows PowerShell: `$env:CRAFT_FONTS_DIR="..\craft-fonts"; $env:CRAFT_FONTS_REQUIRED="1"` before
the same `cargo build`.) At that pin the manifests hold Inter Italic, Carlito/Caladea/Liberation
Italic and Bold Italic, Lato/Merriweather/Poppins Italic, the Montserrat/Open Sans/Roboto/Playfair
Display/Nunito Sans italic variable fonts, and Noto Sans Arabic (variable weight).
Test: `craft_fonts_ship_real_italics_and_the_arabic_fallback` (`crates/fonts`): with no installed
fonts, every required family resolves Italic and Bold Italic to a real italic face, Calibri/Arial
italics reach theirs, and an Arabic cluster in an Inter run falls back to the bundled Arabic face.

## 10. Checkpoints against a hard kill; memory as the OS measures it (round 2)

- SIGTERM, end of input, eviction and panics already auto-save (§1). A hard kill — Windows
  `TerminateProcess`, the OOM killer, a power cut — runs nothing on the way out, so unsaved work is
  now also **checkpointed** while the server runs: a background thread (`crates/mcp/src/checkpoint.rs`)
  writes each presentation with unsaved work to `<name>.recovered-checkpoint-<id>.<ext>` (beside
  the other recovered files: the write root) once it has `--checkpoint-edits` (default 10) edits
  not in the checkpoint yet, or once the oldest of them is `--checkpoint-secs` (default 30) old;
  `0` turns either trigger off. It works from the worker's snapshot of the last finished call, so
  a long call never delays it and the worker never waits for it.
- The checkpoint is replaced in place (temporary file, then rename: never a half-written file, no
  pile of copies). It is removed once superseded — the presentation saved to its file, or a full
  recovered copy written on close, eviction, SIGTERM or the end of input — and kept, and named in
  the next answer, if the presentation goes away with work no other file holds. After a hard kill
  the next run lists it in `recovered_files` with the other recovered files; it opens as a
  presentation. `server_status.checkpoints` shows the triggers and the current checkpoint files.
- Memory: `server_status.memory` (and the `--max-memory-mb` eviction) uses the process's resident
  memory as the OS reports it, not an estimate: the working set from `GetProcessMemoryInfo` on
  Windows, `resident_size` from `task_info(MACH_TASK_BASIC_INFO)` on macOS, `Rss` from
  `/proc/self/smaps` on Linux (crate `memory-stats`), plus the committed size (Windows
  `PagefileUsage`, macOS/Linux virtual size), with `measured` and the `source` named. The crate
  type-checks for `x86_64-pc-windows-gnu` here (`cargo check --target x86_64-pc-windows-gnu -p
  deckcraft-cli`); it was not run on Windows or macOS in this container.
- Tests (`tests/mcp-contract/contract.rs`): `a_hard_kill_loses_little_thanks_to_checkpoints` (3
  edits → checkpoint, not before; more edits replace it in place; SIGKILL; the next run names it and
  it opens with every checkpointed slide; `memory.rss_bytes` is measured),
  `checkpoints_follow_time_and_go_once_the_work_is_saved` (the time trigger; a save removes the
  checkpoint; closing writes the recovered copy and removes the new one).

## 11. Progress on every job (round 2)

Every job — and every running call, in `server_status.current_call` — reports `progress` (0..1,
never going back, 1 only once done), the `stage` it is in, that stage's `steps` (`done`/`total`)
and the `stages` passed so far. Work in several stages names each with the share of the whole it
covers (`deckcraft_engine::task::phase`), so progress runs evenly across them:

| job | stages (share of the whole) and what is counted |
|-----|------------------------------------------------|
| open `.pptx`/`.potx` | reading the file (0–15 %, bytes, 4 MB chunks with a safe point each) → reading the presentation / reading slides (15–90 %, slides) → adding it to the session (90–100 %, when the worker takes it in) |
| open `.deckcraft`, outline | reading the file (bytes) → reading the presentation → adding it to the session |
| export PDF | writing PDF pages (0–90 %, pages, vector or raster) → writing the file (90–100 %, bytes) |
| export PNG/JPEG (all slides) | rendering slides (0–100 %, slides; each file written as its slide is done) |
| export GIF | rendering GIF frames (0–90 %, frames) → writing the file |
| export / save `.pptx`/`.potx`/`.ppsx` | writing slides (0–50 %, slides) → compressing the package (50–90 %, bytes of parts) → writing the file |
| export / save `.deckcraft`, outline | writing the presentation → writing the file |
| `batch` (a call) | running commands (commands) |
| `build_deck` (a call) | slides built |

Every count is also a safe point for `cancel` (§7), except the final file write (the file is
written whole or not at all). Code: `crates/engine/src/task.rs` (`phase`, `Task::overall`,
`Task::stages`), `crates/engine/src/cmd/file.rs`, `crates/engine/src/io.rs`
(`read_file_reporting`, `write_file_reporting`), `crates/pptx/src/write/mod.rs` (`export_with`),
`crates/pptx/src/opc.rs` (`finish_with`), `crates/mcp/src/jobs.rs`. Tests:
`tests_task::every_job_reports_its_stages_and_progress` (engine: every export format, save, and
opening `.pptx` and `.deckcraft`), `task::tests::phases_map_their_steps_into_the_whole_and_never_go_back`,
and `long_work_runs_as_a_job` (contract: PDF, PNG, `.pptx` export jobs and an open job report
their stages; the open job is seen in "reading the file" while it runs).

## Parity pass (2026-10-18): one contract surface for the three craft forks

WordCraft, GridCraft and DeckCraft were built independently against the same contract; this pass
read the three MCP servers side by side and gave them one shared surface. **`CONTRACT-PARITY.md`**
(the same file in the three repositories) is the table: every contract item, what each app did
before, and the one shape all three answer with now. Where an earlier section of this file says
otherwise (an error code, a field name, a flag, a default), the parity pass and
`CONTRACT-PARITY.md` are what holds. Domain tools keep their names.

| Change | Ported from | Where | Tested by (`tests/mcp-contract/contract.rs` unless noted) |
|---|---|---|---|
| Error codes from the shared set of 18 (`contract::code::ALL`): `forbidden` → `outside_root` (with both roots), and `locked`, `disk_full`, `password_required`, `wrong_password`, `not_available`, `invalid_request` classified; `candidates` → `closest`; JSON-RPC errors carry `{code, reason, next_step}` in `data`; arguments that are not an object answer `invalid_argument` (were ignored) | WC, GC | `crates/mcp/src/contract.rs`, `server.rs`, `runtime.rs` | every answer: `common::Mcp::check` checks the code is in the shared list; `errors_have_a_code_a_reason_and_a_next_step` |
| A call still running after `--job-after-ms` (15000), or sent with `async: true` (any tool), or one the client gave up on (`notifications/cancelled`), carries on as a job: its caller gets `{job_id, status: "running"}` and the answer (pictures included) waits in `job_status` — a client timeout no longer loses a long `build_deck` or `batch` | WC, GC | `runtime.rs` (`hand_off`, worker, watchdog), `state.rs` (`current_request`, `new_job_for`, `job_answered`) | `a_long_call_becomes_a_job_whose_result_survives` |
| `job_status` in the shared shape: `status` (was `state`), `tool` (with the old `kind`'s meaning), `progress`, `stage`, `steps`, `stages`, `result` or `error` (a cancelled job's error too); `list_jobs` and `job_status {}` answer `{jobs}` (was a bare array) | WC, GC | `state.rs` (`Job::to_value`), `runtime.rs` | `long_work_runs_as_a_job` |
| `cancel` answers `{cancelled, status: cancelling\|not_running\|…, job_id, tool, next_step}`; a cancelled `batch` puts the presentation back as it was before the call (the commands before the stop no longer stay) | WC, GC (rollback) | `runtime.rs`, `tools.rs` (`batch`) | `a_running_call_stops_at_a_safe_point_when_cancelled` (a 30 000-command batch cancelled: one slide left) |
| Progress notifications: a request with `_meta.progressToken` gets `notifications/progress` while it runs | WC | `runtime.rs` (`progress_notification`) | `a_call_with_a_progress_token_gets_progress_notifications` |
| Checkpoints write the recovered file itself, `<name>.recovered-<YYYYMMDD-HHMMSS>.<ext>` (was a separate `<name>.recovered-checkpoint-<id>.<ext>`); an auto-save of the same stretch of work (close, eviction, SIGTERM, end of input) brings that file up to date instead of writing a second one; `server_status.checkpoints` gains `pending_edits` and `writes`; the start-up scan skips a crash's `.saving` file | WC, GC | `checkpoint.rs` (`file_name`, `take_path`, `status`), `autosave.rs` (`write_recovered`, `free_name`, `scan`) | `a_hard_kill_loses_little_thanks_to_checkpoints`, `checkpoints_follow_time_and_go_once_the_work_is_saved` |
| `server_status` in the shared shape: `documents` with `name`, `current_call` with `job_id` and the progress fields, `memory {rss_bytes, committed_bytes, limit_bytes, measured, source}` (was `rss_mb`, `limit_mb`), `fonts {source, faces, families, default, arabic}`, `evicted [{name, reason, recovered_path, original_path}]`, `recovered_files [{path, reason, original, at}]`, `limits.job_after_ms`; errors and answers use `recovered_path` / `original_path` (were `recovered`, `reopen`) | WC, GC | `server.rs` (`status_value`, `fonts_value`), `state.rs` | `server_status_has_the_shared_shape`, `eviction_auto_saves_and_the_next_call_names_the_file` |
| Defaults: `--busy-after-ms` 300 (was 8000: a waiting call answered `busy` only after 8 s), `--open-job-mb` 20 (was 24; decimals accepted), checkpoints every 20 edits / 60 s (were 10 / 30); `--max-memory-mb` no longer floors at 16 | WC, GC | `state.rs` (`Config`), `apps/deckcraft-cli/src/main.rs`, `help.rs` | the suite passes the flags it needs |
| Every tool takes `async`, `continue_from` (on list tools read as `offset`) and `max_bytes`; enum values in any case; a list given as `"a, b"` | WC, GC | `tools.rs` (`tool`), `contract.rs` (`coerce`, `COMMON_ARGS`), `server.rs` (`finish`) | `oversized_results_are_trimmed_and_continue`, `contract::tests` |
| OLD_ renames also say so in `warnings` | WC, GC | `server.rs` (`finish`) | `saves_keep_the_previous_file_as_old` |
| `roundtrip_check`: open + save a deck in memory and report what a save would drop — parts, relationships, content types, binary parts, each XML part's element inventory — with WordCraft's report keys | WC | `crates/mcp/src/roundtrip.rs` (new, the same as GridCraft's), `server.rs`, `tools.rs` | `roundtrip_check_reports_what_a_save_would_keep`, `roundtrip::tests`, `smoke.py` |
| Annotations: `evict` idempotent; `roundtrip_check` read-only | — | `tools.rs` (`hints`) | `annotations_tell_reads_from_writes` |
| The contract suite takes `DECKCRAFT_CLI` (another binary to test), as WordCraft's and GridCraft's take theirs | WC, GC | `tests/mcp-contract/common/mod.rs` | the release workflow |
| `whiq-release.yml` and `engines.lock.example` (below) | — | `.github/workflows/whiq-release.yml`, `engines.lock.example` | the workflow runs the contract suite and `smoke.py` on the built binary |

### Fonts: one craft-fonts pin for the three forks

DeckCraft already pinned craft-fonts **`43913056ce82a3fdad0ef69f6b88d1dc922a8104`** (§9); WordCraft
and GridCraft pin it now too, and `whiq-release.yml` checks it out.

### Release workflow

`.github/workflows/whiq-release.yml` (new; the upstream `release.yml` is untouched): on a tag
`whiq-v*` (or Run workflow, with an optional tag to publish) it builds only `deckcraft-cli`, for
`x86_64-unknown-linux-musl` (static: the host's glibc does not matter) on ubuntu-22.04 and
`x86_64-pc-windows-msvc` (static C runtime) on windows-2022, with craft-fonts checked out at the
pin and `CRAFT_FONTS_REQUIRED=1`; runs the contract suite (`mcp_contract`, `mcp_deck`) against the built binary on both (`DECKCRAFT_CLI`), and
`smoke.py` (on Windows too) plus a "statically linked" check on Linux; and publishes a GitHub release with
`deckcraft-cli-<tag>-linux-x86_64.tar.gz`, `deckcraft-cli-<tag>-windows-x86_64.zip` (binary, licences,
the embedded fonts' licences, `CONTRACT-PARITY.md`, `WHIQ-CHANGES.md`, `BUILD.txt`), `SHA256SUMS`
and `engines.lock`. Actions are pinned by commit (checkout, rust-toolchain, rust-cache, as in
upstream CI) or by major (upload/download-artifact); no signing; no secrets (the job's own
`GITHUB_TOKEN` publishes). `engines.lock.example` at the root shows the JSON the platform's build
script reads: a list of `{app, tag, file, sha256}`, one per archive; the release's `engines.lock`
is that file with the real hashes.


### Test status after the parity pass (Linux x86_64, 2026-10-18, craft-fonts 43913056, `CRAFT_FONTS_REQUIRED=1`)

- `cargo build --release -p deckcraft-cli` succeeds; against the release binary
  (`DECKCRAFT_CLI=target/release/deckcraft-cli cargo test --release -p deckcraft-cli`): `mcp_contract`
  21/21 (17 before, plus the call-job, `roundtrip_check`, `server_status` shape and progress
  notification tests), `mcp_deck` 7/7, and the CLI's other tests; `smoke.py` passes on it.
- `deckcraft-mcp` and `deckcraft-engine`: 358 tests pass, 0 fail, 1 ignored; with the CLI, 410 pass.
  `cargo clippy --no-deps -D warnings` on `deckcraft-mcp` / `deckcraft-cli` passes. (A plain
  `cargo clippy` stops at an upstream lint in `crates/model/src/trendline.rs:468`, "manual
  `!Range::contains`", which Rust 1.97's clippy reports and the parity pass did not touch.)
- musl: `cargo build --release --locked -p deckcraft-cli --target x86_64-unknown-linux-musl` gives a
  `static-pie linked` binary; `mcp_contract` (21/21), `mcp_deck` (7/7) and `smoke.py` pass on it.

## Platform pass (2026-10-20): OLD_ off, structured errors everywhere, file arguments

| Change | Where | Tested by (`tests/mcp-contract/contract.rs`) |
|---|---|---|
| `--automation-no-old-copies`: saves, exports and `build_deck` replace files, with no `OLD_` copy and no `previous_files_renamed` (the platform keeps the backups, and no longer needs to pass `overwrite: true` on every call); `server_status.limits.old_copies` says which. Without the flag nothing changes. | `apps/deckcraft-cli/src/main.rs`, `help.rs`, `crates/mcp/src/state.rs` (`Config::keep_old`), `server.rs` | `old_copies_can_be_turned_off_for_a_supervisor` |
| A line that is not JSON was answered with a bare `parse error` message; it now carries `{code, reason, next_step}` in `error.data` like every protocol error. A batch's failed steps (`results[].error`, stopped or with `keepGoing`) were the engine's message as a string; they are now the error object. | `crates/mcp/src/runtime.rs`, `server.rs` (`structured_steps`) | `errors_have_a_code_a_reason_and_a_next_step` |
| `tools/list` names the files a tool reads: `_meta: {"whiq/files": {"read": [argument names], "opens": bool}}` on `open_presentation` (`path`, opens), `new_presentation` (opens), `build_deck` (`template`, opens; its `path` is the output), `adopt_template`, `roundtrip_check` and `insert_picture` (`path`). The same key in every craft app (CONTRACT-PARITY.md). | `crates/mcp/src/tools.rs` (`files_meta`) | `annotations_tell_reads_from_writes` |

Also: a `collapsible_if` that Rust 1.97's clippy reports in the contract client (`plain_path`) is collapsed.

Test status (Linux x86_64, craft-fonts 43913056): `cargo test --workspace --exclude deckcraft
--no-fail-fast` (the desktop app needs ALSA headers this container lacks): every test passes but the
same 12 `deckcraft-ui-egui` ones that need a GPU ("No adapter found"); `long_work_runs_as_a_job` is
timing-sensitive under a loaded machine (failed once, passed on the next runs). `cargo clippy
--no-deps -p deckcraft-mcp -p deckcraft-cli --all-targets -- -D warnings`, `cargo fmt --check`,
`cargo xtask layers`, `assets` and `wasm` pass (a workspace clippy still stops at the upstream lint in
`crates/model/src/trendline.rs:468`, below). `cargo build --release --locked -p deckcraft-cli`
(`CRAFT_FONTS_REQUIRED=1`) succeeds; against that binary `mcp_contract` passes 22/22, `mcp_deck`
7/7, and `smoke.py` passes.

## Not done / known limits

- By design, not a gap: busy is per process — one worker serves calls in order (one process per
  caller and document is the platform's model); a running call stops at its next safe point on
  `cancel` (§7), and long work runs as jobs alongside it.

## Test status (Linux x86_64 container, 2026-10-17, after round 2)

- Release binary, built with the release recipe (§9: `CRAFT_FONTS_DIR` at craft-fonts `43913056`,
  `CRAFT_FONTS_REQUIRED=1`): `cargo test --release -p deckcraft-cli --test mcp_contract --test
  mcp_deck` — 24 passed (17 contract, 7 deck); `python3 tests/mcp-contract/smoke.py` passed.
- `cargo test --no-fail-fast` over every crate plus `deckcraft-cli`: 1648 passed, 12 failed, 6
  ignored. All 12 failures are in `deckcraft-ui-egui` and have one cause, `Failed to create render
  state: CustomNativeAdapterSelectionError("No adapter found")` (egui_kittest needs a GPU; the
  container has none). The desktop app `apps/deckcraft` does not build here (no ALSA headers for
  `alsa-sys`). Everything the headless binary uses is green.
- `cargo check --target x86_64-pc-windows-gnu -p deckcraft-cli` passes (the Windows memory and
  checkpoint code type-checks); it was not run on Windows or macOS.
