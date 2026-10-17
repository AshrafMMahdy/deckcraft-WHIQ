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
| §10 bidi | A paragraph with no direction of its own takes its base direction from its first strong character (UAX #9 P2/P3): Arabic is RTL and right-aligned unless the paragraph aligns itself — in layout (`crates/text`) and in the saved file (`a:pPr rtl="1" algn="r"`, `crates/pptx` writer), so PowerPoint shows it the same. Arabic shaping and the Arabic fallback font come from upstream (harfrust, Noto Sans Arabic via craft-fonts); PDF export is raster today. | `crates/text/src/lib.rs` (`shape_para`), `crates/pptx/src/write/dml.rs` (`paragraph`) | `arabic_paragraphs_are_right_to_left_by_default` |

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
