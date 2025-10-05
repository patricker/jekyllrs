Got it—no runtime fallbacks to Ruby, no “compat flags,” and `jekyllrs` is a **replacement**, not a sibling. Serve/watch is an early pickup. Rendering is the long pole. Below is a **concrete, phased TODO** that reflects those constraints and what’s already in your tree (`rust/jekyll-core`, `rust/jekyll-cli`, Ruby bridge in `lib/jekyll/rust.rb`, etc.).

---

## Phase 1 — Entrypoints & packaging (Rust by default, everywhere)

**Goal:** `jekyll` and `jekyllrs` both execute the Rust engine (not “if available”—**always**). Remove env‑var coupling and Linux‑only dlopen assumptions.

**TODO**

* **Load the cdylib without env vars**

  * [x] In `lib/jekyll/rust.rb`, switch `ensure_loaded!` to `require 'jekyll_core'` first; only then allow `JEKYLL_RUST_LIB` override as a dev escape hatch. Remove the hard error when env var is missing.
    *Files:* `lib/jekyll/rust.rb`
  * [x] Create a tiny Ruby extension gem wrapper for the cdylib (`ext/jekyll_core`) so `require 'jekyll_core'` works cross‑platform. Use `rb-sys` template to build the Rust `cdylib`.
    *Files:* new `ext/` scaffold + gemspec changes.

* **Cross‑platform library discovery for `jekyllrs`**

  * [x] In `rust/jekyll-cli/src/main.rs`, make `locate_rust_lib()` probe `.so/.dylib/.dll` and both `libjekyll_core.*`/`jekyll_core.*`.
    *Files:* `rust/jekyll-cli/src/main.rs`
  * [x] Update `script/rust-build` to compute `LIB_EXT` per OS and echo the correct `JEKYLL_RUST_LIB=…`.
    *Files:* `script/rust-build`

* **CLI wiring (no Mercenary dependence for `jekyllrs`)**

  * [x] Keep your Rust arg parser in `jekyllrs` and ensure `--trace`, config selection, multiple `-c`/`--config`, and `--profile` semantics match.
    *Files:* `rust/jekyll-cli/src/main.rs`

* **Packaging**

  * [ ] Ship prebuilt native gems for common platforms (macOS x64/arm64, Linux x64/aarch64 gnu+musl, Windows x64).
    *Build:* GH Actions matrix; cache cargo.

**Acceptance**

* `bundle exec jekyll build` engages Rust with **no env var** on macOS, Linux, Windows.
* `jekyllrs build` works on all three OSes and finds the lib next to the binary.
* Removing Rust (or breaking the lib) **breaks the command** (no fallback).

---

## Phase 2 — Serve & Watch in Rust (early win + reduce bridging)

**Goal:** Replace Ruby’s `Serve`/`jekyll-watch` with Rust implementations, but still trigger Ruby plugins/hooks where expected.

**TODO**

* **HTTP server**

  * [x] Implement a minimal static server in Rust (e.g., `hyper` or `axum`) with:

    * [x] directory index support (toggle via config),
    * [x] gzip/deflate/BR if requested,
    * [x] Cache-Control + 404 fallback responses (500/TLS TODO),
    * [x] baseurl handling.
  * [x] Map Jekyll config → server settings (port/host/ssl options) per `lib/jekyll/commands/serve.rb` options.

* **LiveReload**

  * [x] Inject LiveReload script (Rust) or surface interim guidance if Injector missing.
  * [x] WebSocket endpoint in Rust mirroring `livereload` semantics.
  * [x] Implement `livereload_ignore` filtering identical to Ruby (`File.fnmatch?` parity). You can call back into Ruby initially (existing RRegexp/`fnmatch` bridge), then replace with a Rust `globset` implementation that matches Ruby’s flags.
  * [x] Remove Ruby hook registration; broadcast post-build reloads directly from the Rust engine.



* **Watch**

  * [x] Use `notify` (debounced) with ignore rules honoring: `exclude`, `_site`, theme/vendor dirs, `.jekyll-metadata`, etc.
  * [x] On change: call **Rust build** (`engine_build_process`) and then broadcast LiveReload diffs via the Rust LiveReload bridge.

* **Command surface**

* [x] Add `serve` subcommand to `jekyllrs` (you already parse `serving`/`watch` inside `cli_build.rs`; use a native watcher path instead of `Jekyll::Watcher`).
    *Files:* `rust/jekyll-cli/src/main.rs`, new `rust/jekyll-core/src/cli_serve.rs` (or fold into `cli_build.rs`).
  * [x] Register `jekyll serve` via `Jekyll::CLI::ServeCommand` so the Ruby CLI just shells into the Rust bridge.
    *Files:* `exe/jekyll`, `lib/jekyll/cli/serve_command.rb`

* **Remove Ruby watchers from hot path**

* [x] Stop requiring `jekyll-watch` in `cli_build.rs` (currently invoked when `watch` true). Replace with noop there; watcher lives in Rust serve path.
    *Files:* `rust/jekyll-core/src/cli_build.rs`

**Acceptance**

* `jekyll serve` and `jekyllrs serve` start the Rust server; file changes rebuild and reload on all three OSes.
* Cucumber serve/watch behaviors pass (same flags, same user‑visible behavior).
* No Ruby `Watcher` threads spawned.

---

## Phase 3 — Rendering orchestration in Rust (keep Ruby Liquid for the moment)

**Goal:** Move all orchestration (layout chain, payload shaping, document sequencing) into Rust; keep **actual Liquid evaluation** in Ruby for this phase. No runtime switch—remove old Ruby orchestration as you land Rust.

**TODO**

* **Layout chain & payload**

  * [x] Build the layout resolution and rendering order in Rust (`engine.rs`) and call Ruby Liquid once per step.
  * [x] Create Rust structs for site/page/post payloads and only convert to Ruby once per render call (minimize object churn).
    *Files:* `rust/jekyll-core/src/engine.rs` (extend), new payload module.

* **Includes**

  * [x] Resolve `{% include %}` and `{% include_relative %}` paths in Rust; pass final strings/data into Ruby Liquid.

* **Filters registry**

  * [x] Register Ruby filters **once** per render cycle in a hub (see next phase), not per page.

* **Remove duplicated logic in Ruby**

  * [x] Delete/mothball Ruby code in `lib/jekyll/renderer.rb` that orchestrates phases already handled in Rust (keep only compatibility shims that call the Rust bridge).

**Acceptance**

* Rendering correctness tests pass against your current suite, with render time reduced vs. current master due to fewer Ruby crossings.

---

## Phase 4 — Liquid engine in Rust (with Ruby filter/tag bridge)

**Goal:** Replace Ruby Liquid with a Rust Liquid implementation and **bridge** to Ruby for user‑defined filters/tags. This is not a fallback—Ruby filters/tags are a **first‑class** part of the Rust engine via a stable FFI boundary.

**TODO**

* **Engine**

  * [x] Integrate `liquid` (Rust) and extend in‑tree.
  * [x] Implement core Jekyll behaviors in Rust engine:
    - `strict_filters`/`strict_variables` respected (delegates to Ruby Liquid when strict),
    - `{% highlight %}` passthrough via synthetic block,
    - template caching keyed by path/mtime/filters.
    - Note: whitespace trim semantics tracked separately if diffs arise in tests.

* **Ruby bridge for filters/tags**

  * [x] Filters: for filters not implemented in Rust, marshal args to Ruby and run the Ruby filter (arity resolved by Liquid), fast‑path common scalars/arrays/maps.
  * [x] Tags: tag provider that defers to Ruby’s tag class with body capture when needed.
  * [x] Implement core Jekyll filters natively in Rust (`where`, `where_exp`, `sort`, `group_by`, URL helpers) and keep the bridge for everything else.
    - Implemented: `where`, `where_exp`, `sort` (with `nils:` positional and keyword), `group_by`, `find`, URL helpers (`absolute_url`, `relative_url`, `strip_index`), and collection utilities (`uniq`, `compact`).
    *Files:* `rust/jekyll-core/src/liquid_engine.rs`, `rust/jekyll-core/src/utils.rs`.

* **Drop semantics**

  * [x] Convert Ruby Drops/Hashes to Liquid values with cycle‑guard; expose stable projections for `SiteDrop`/`DocumentDrop` keys while deferring heavy keys.
  * [ ] Optional: `RubyDrop` adapter implementing `ValueView` with on‑demand forwarding when needed.

* **Template caching**

  * [x] Cache parsed templates (keyed by path + mtime + filters); invalidate on path/mtime change; cap cache size.

* **Remove Ruby Liquid**

  * [ ] Remove Ruby Liquid from render path entirely. Note: current engine delegates only for strict modes and unknown filters; remove remaining delegation once native filters/tags cover gaps.

**Acceptance**

* All render‑related features pass. Any remaining differences are addressed in Rust (no “flip back to Ruby Liquid” escape).

---

## Phase 5 — Converters pipeline in Rust (wrapping Ruby converters/plugins)

**Goal:** Orchestrate conversion in Rust; **call Ruby converters/plugins through a dedicated bridge**, but the pipeline control lives in Rust.

**TODO**

* **Converter registry in Rust**

  * [x] Discover Ruby converters (classes responding to `matches`/`convert`) once at startup and capture their priority.
  * [x] For each input ext, pick converter chain in Rust and invoke Ruby converters sequentially.
  * [ ] Inline Rust implementations where you want speed: optional Markdown (`comrak`/`pulldown-cmark` with kramdown-compat shims), optional syntax highlight (`syntect`). (These are not runtime flags—pick an implementation and delete the Ruby equivalents when ready.)
    - [~] Registered a `markdown: rust` converter shim (delegates to Kramdown for now; no new dependencies). Ready to swap for a native renderer when dependencies are allowed.
    *Files:* `rust/jekyll-core/src/render.rs`.

* **Sass**

  * [ ] Keep calling `jekyll-sass-converter` initially; later, consider `grass` with parity layer.

**Acceptance**

* Converter tests pass. Measured drop in per‑page render time due to fewer Ruby crossings.

---

## Phase 6 — Reader & data pipeline completion (finish removing Ruby from I/O)

**Goal:** Eliminate remaining Ruby in the hot I/O path.

**TODO**

* **Front matter & YAML**

  * [x] Port front matter parsing from `SafeYAML` calls to Rust (`serde_yaml`) with the same rules (BOM handling already in `yaml_header.rs`; preserve booleans, dates, and aliases semantics).
    *Files:* `rust/jekyll-core/src/document_reader.rs` (replace SafeYAML call)
  * [x] Normalize date/time parsing to match Ruby’s behavior (your `dates.rs`/`time_utils.rs` can centralize this).

* **Reader**

  * [x] Walker/classifier parity; `EntryFilter` parity for dotfiles/`exclude` rules implemented fully in Rust; removed Ruby filtering branches.
    *Files:* `rust/jekyll-core/src/entry_filter.rs`, `fs_walk.rs`, `reader.rs`

* **Static file writes**

  * [x] Ensure single-file writes use tmp+rename, permission mirroring, and mtime updates across OSes.
    *Files:* `rust/jekyll-core/src/static_file.rs`
  * [x] Ensure batch writer reuses atomic semantics and reapplies mtimes after parallel copy.
    *Files:* `rust/jekyll-core/src/static_file.rs`

**Acceptance**

* All cucumber “read/scan/write” features green across OS matrix.

---

## Phase 7 — Plugin hook hub & data model stabilization

**Goal:** Centralize all plugin calls in one Rust module; minimize Ruby object construction and ensure ordering matches Jekyll.

**TODO**

* **Hook hub**

  * [ ] A single Rust module that fires `:pre_render`, `:post_render`, `:post_write`, generators, etc., in the exact order.
  * [ ] Maintain object identity where plugins expect it; cache Ruby wrappers for frequently accessed Rust structs.

* **Profiling**

  * [x] Attribute timings to each plugin/hook; surface summary at the end of build (`--profile`).
    - Implemented aggregation and summary table (enable via `config['profile_hooks']` or debug logging).
    *Files:* `lib/jekyll/rust.rb`, `rust/jekyll-core/src/engine.rs`.

* **Generators**

  * [x] Drive Ruby generators from Rust; ensure new pages/documents they create are fed back through the Rust pipeline.
    - Implemented timing + invocation from Rust; outputs re-rendered in Rust pipeline.
    *Files:* `rust/jekyll-core/src/engine.rs`.

**Acceptance**

* Popular plugins (pagination, feeds, SEO, Sass) pass their tests unmodified.

---

## Phase 8 — Test, perf, and platform polish

**Goal:** Lock correctness and squeeze render time.

**TODO**

* **Dual-run harness (build vs. serve)**

  * [ ] Run the full cucumber suite in CI for macOS/Linux/Windows (Ruby 3.1–3.3).
  * [ ] Add a large “real‑world” site fixture; record wall time and top hotspots per phase.

* **String/Path interning**

  * [ ] Intern common strings (permalinks, keys) and normalized paths to reduce allocations across the render pass.

* **Parallelism (careful)**

  * [ ] Optional: render pages in parallel if plugins are thread‑safe. Default to 1; add a single config knob in code (not a user‑visible “compat” flag).

* **Logging/trace**

  * [ ] Standardize on `tracing` crate in Rust. When `--trace`, set `RUST_BACKTRACE=1` (you already partially do this) and include Ruby backtraces for bridged errors.

**Acceptance**

* Stable CI times; a documented delta vs. pre‑Phase‑3 baseline, with flamegraphs for “render” showing Rust Liquid taking the bulk.

---

## Phase 9 — Cleanout & release cut‑over

**Goal:** Remove dead Ruby code and publish the replacement package.

**TODO**

* **Delete superseded Ruby**

  * [x] Drop the Ruby serve command and WEBrick helpers now that CLI delegation lives in Rust.
    *Files:* `exe/jekyll`, `lib/jekyll/cli/serve_command.rb`, removed `lib/jekyll/commands/serve*`
  * [ ] Remove Ruby renderer orchestration and any remaining helpers now handled in Rust.
    *Files:* `lib/jekyll/renderer.rb`

* **`jekyllrs` as the replacement package**

  * [ ] Publish a `jekyllrs` gem that **provides the `jekyll` executable** (and optionally a `jekyllrs` alias) and depends on the native `jekyll_core` extension.
  * [ ] Mark explicit **conflict** with the legacy `jekyll` gem so users “swap,” not co‑install.
  * [ ] Keep the Ruby plugin surface stable (same `Jekyll::Plugin`/hooks API).

* **Docs**

  * [ ] Update “Getting Started” to say: install `jekyllrs` (or Docker image) and keep existing plugins.

**Acceptance**

* Fresh machine install with only `jekyllrs` runs `jekyll build/serve` using your Rust core.
* No code path exists that can “fall back” to Ruby implementations of core phases.

---

## Bite‑size tasks you can land immediately

* [x] **Mac/Windows dlopen** in `jekyllrs` (`.dylib`/`.dll`) and `script/rust-build` OS detection.
* [x] `--trace` ⇒ set `RUST_BACKTRACE=1` in `rust/jekyll-cli/src/main.rs`.
* [x] Move the **watch** decision entirely out of Ruby by deleting the `jekyll-watch` calls in `cli_build.rs` and stubbing them until Phase 2 server lands.
* [x] Add a **Liquid hot-path benchmark** (render N posts with layouts and includes) to CI to track progress through Phases 3–4.
  * Script added: `benchmark/build-small-site.rb` to generate a small site and time `site.process` via Rust engine.
* [x] In `entry_filter.rs`, finish the Rust‑side `fnmatch` parity and remove Ruby `RRegexp` reliance after tests are green.

---

## Notes keyed to current code

* Your `engine_build_process` (Rust) already drives phases and watcher decisions; redirect all watch/server logic there once the Rust server exists (`cli_build.rs` → no `Jekyll::Watcher`).
* YAML: `yaml_header.rs` has header detection; `document_reader.rs` still requires `safe_yaml`—that’s the choke‑point to replace.
* You already have fast filters (`group_by_fast`, `where_*_fast`, etc.) and path/url/slugify in Rust. They can be reused by the Rust Liquid engine as **native filters**.
