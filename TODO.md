# Jekyll Rust Engine — Verified Status & TODO

> **Last verified:** 2026-03-01 (against actual source code, not previous notes)

---

## Cucumber Test Correctness

### Verified Fixes (in code)

1. **`render.rs` error handler (line 1454)** — Returns empty string on "Unknown index" in lax mode ✅
   - Code confirmed at `render.rs:1454`: `return Ok(ctx.ruby.str_new("").into_value_with(ctx.ruby));`

2. **`to_liquid_value()` eagerly resolves `excerpt`** — With `IN_EAGER_RESOLVE` recursion guard ✅
   - Code confirmed at `liquid_engine.rs:682-740`

3. **`SafeValue::get()` returns nil for non-Object types in lax mode** ✅
   - Code confirmed at `liquid_engine.rs:892`: `else if self.strict { None } else { Some(&self.nil) }`

4. **`SafeValue::contains_key()` returns true for non-Object types in lax mode** ✅
   - Code confirmed at `liquid_engine.rs:841`: `_ => if self.strict { false } else { true }`

### Known Bug: strict mode key mismatch

**`build_render_info` uses symbol keys** (`:strict_filters`, `:strict_variables`) to store in the info hash, but **`render_liquid_template` reads with string keys** (`"strict_filters"`, `"strict_variables"`). This means the primary read always fails, and only the fallback path (direct site.config.liquid read, lines 1396-1405) works. This should be fine for correctness but wastes FFI calls.

- `build_render_info` at `render.rs:1347-1348`: symbol keys
- `render_liquid_template` at `render.rs:1392-1393`: string keys

### Debug Output Still Present ⚠️

Two debug `eprintln!` statements remain:
- `render.rs:1433` — `eprintln!("RUST_DEBUG render_liquid_template error: ...")`
- `render.rs:1455` — `eprintln!("RUST_DEBUG lax unknown index: ...")`

These should be removed before benchmarking/release.

### Test Status

**Last known:** 14 failures (down from 20). Not re-verified in this review.
```
STILL FAILING (14 tests — last known, needs re-run to confirm):
  collections.feature:424         — sort by title
  create_sites.feature:204        — related posts
  include_tag.feature:6, 108      — include params (still hitting Ruby path)
  incremental_rebuild.feature:55, 70 — incremental rebuild
  markdown.feature:6, 20          — for loop content/excerpt
  post_data.feature:383           — page.next.title / page.previous.title
  rendering.feature:66, 85        — strict mode not propagating error to exit code
  rendering.feature:211           — page.content in for loop
  site_data.feature:79, 90        — site.tags iteration
```

---

## Phase 1: Native Include Tag — Status

### Step 1a: Copy Jekyll include tag — PARTIALLY DONE (via different approach)

- `liquid-lib` with `jekyll` feature **IS** in Cargo.toml dependencies (`liquid-lib = { version = "0.26", features = ["jekyll"] }`)
- However, there is **no `JekyllIncludeTag`** registered in the parser builder
- Instead, includes are handled via `RubyTagRenderable::render_to()` fast-path (lines 2842-2864): when `self.name == "include"` and the markup is a simple filename (no params, no `{{`, no spaces), it uses `runtime.partials().get(trimmed)` to render via the Rust PartialStore
- **Complex includes with params** still go through Ruby

### Step 1b: JekyllIncludeSource (PartialSource) — ✅ COMPLETE

- `JekyllIncludeSource` struct implemented at `liquid_engine.rs:237-274`
- Implements `PartialSource` trait with `contains()`, `names()`, `try_get()`
- `try_get()` includes preprocessing via `preprocess_raw_tag_markup()` ✅
- Includes symlink safety check for safe mode ✅

### Step 1c: Wire into parser builder with LazyCompiler — ✅ COMPLETE

- `fetch_include_config()` fetches include dirs from Ruby at `liquid_engine.rs:277-314`
- `LazyCompiler::new(include_source)` created at line 3303
- Parser built with `.partials(partials)` at line 3306
- Parser is cached in `PARSER_CACHE` thread_local ✅

### Step 1d: Remove "include" from preprocessor needs_raw — ❌ NOT DONE

- `include` is **still** in the `needs_raw` list at line 65:
  ```rust
  let needs_raw = matches!(name.as_str(), "post_url" | "include" | "include_relative" | "link");
  ```
- This means `{% include %}` markup is hex-encoded before the parser sees it
- The native include fast-path in `RubyTagRenderable::render_to()` decodes the hex first, then does the partial lookup
- This works but is suboptimal — the ideal approach would remove `include` from `needs_raw` and register a proper `JekyllIncludeTag` that parses the raw syntax directly

### Step 1e: Skip RubyTagParser for "include" — ❌ NOT DONE

- At line 3345-3350, ALL tag names from `fetch_tag_kinds()` get registered as `RubyTagParser` (except `assign`)
- `include` is NOT excluded — it gets a `RubyTagParser` which overrides whatever the stdlib registered
- The fast-path works only because `RubyTagRenderable::render_to()` intercepts `include` before calling Ruby

### Current Include Architecture (actual, not planned)

```
Rust preprocessor hex-encodes {% include filename.html %}
  → RubyTagParser("include") parses hex-encoded markup
  → RubyTagRenderable::render_to() is called
    → Decodes hex markup
    → If simple (no params, no {{, no spaces):
        → Looks up partial via runtime.partials() (LazyCompiler → JekyllIncludeSource)
        → Renders partial in Rust ✅
    → If complex (params/variables):
        → Falls through to Ruby FFI ❌ (still slow)
```

**Impact:** Simple parameterless includes (the majority for www.ruby-lang.org) render in Rust. Includes with parameters still go through Ruby.

---

## Phase 2: Native Link Tag — COMPLETE (via hex-encoded approach)

### NativeLinkTag struct exists but is NOT registered in the parser builder

- `NativeLinkTag` defined at `liquid_engine.rs:364-440` with full `ParseTag` implementation
- **NOT registered** in the parser builder — it's never called
- `link` is still in `needs_raw` (hex-encoded in preprocessor)
- Instead, `RubyTagRenderable::render_to()` has a fast-path for `link` at lines 2867-2894 that does the lookup from `LINK_TABLE`

### Link lookup table — ✅ COMPLETE

- `LINK_TABLE` thread_local cache at line 322
- `get_link_table()` populates from Ruby via `link_lookup_table` method (one-time, cached)
- Called at line 3380 during template rendering
- Supports exact match, with/without leading slash

**Net result:** Link tags work natively in Rust via the hex-encoded path. The `NativeLinkTag` struct is dead code.

---

## Native Filters — Status

### Verified as registered in parser builder (lines 3311-3339):

| Filter | Status | Notes |
|--------|--------|-------|
| `map` | ✅ Native | `MapFilterParser` |
| `join` | ✅ Native | `JoinFilterParser` |
| `where` | ✅ Native | `WhereFilterParser` |
| `where_exp` | ✅ Native | `WhereExpFilterParser` |
| `sort` | ✅ Native | `SortFilterParser` |
| `group_by` | ✅ Native | `GroupByFilterParser` |
| `find` | ✅ Native | `FindFilterParser` |
| `absolute_url` | ✅ Native | `AbsoluteUrlFilterParser` |
| `relative_url` | ✅ Native | `RelativeUrlFilterParser` |
| `strip_index` | ✅ Native | `StripIndexFilterParser` |
| `uniq` | ✅ Native | `UniqFilterParser` |
| `compact` | ✅ Native | `CompactFilterParser` |
| `xml_escape` | ✅ Native | `XmlEscapeFilterParser` |
| `cgi_escape` | ✅ Native | `CgiEscapeFilterParser` |
| `uri_escape` | ✅ Native | `UriEscapeFilterParser` |
| `normalize_whitespace` | ✅ Native | `NormalizeWhitespaceFilterParser` |
| `number_of_words` | ✅ Native | `NumberOfWordsFilterParser` |
| `jsonify` | ✅ Native | `JsonifyFilterParser` |
| `array_to_sentence_string` | ✅ Native | `ArrayToSentenceStringFilterParser` |
| `push` | ✅ Native | `PushFilterParser` |
| `pop` | ✅ Native | `PopFilterParser` |
| `shift` | ✅ Native | `ShiftFilterParser` |
| `unshift` | ✅ Native | `UnshiftFilterParser` |
| `to_integer` | ✅ Native | `ToIntegerFilterParser` |
| `inspect` | ✅ Native | `InspectFilterParser` |
| `markdownify` | ✅ Native | `MarkdownifyFilterParser` (comrak) |

**Total native: 26 filters**

### Still Ruby-only (not found in Rust source):

- `date_to_string`, `date_to_long_string`, `date_to_xmlschema`, `date_to_rfc822`
- `smartify`, `sassify`, `scssify`
- `slugify` (exists in `slugify.rs` but NOT wired as a Liquid filter)
- `sample`, `find_exp`, `group_by_exp`

---

## Build Tooling — Status

### `script/rust-build` — ✅ COMPLETE

- Script exists at `script/rust-build` (35 lines)
- Supports `release` (default) and `debug` modes
- Builds Rust extension and copies to `lib/jekyll_core.so`

### Native Markdown converter (comrak) — ✅ COMPLETE

- `RustMarkdownNativeConverter` in `render.rs` (line 252+)
- Uses comrak with config mapped from Jekyll's kramdown options
- Registered as an alternative to Ruby's Kramdown converter

---

## Remaining Work (Priority Order)

### Immediate

1. **Remove debug `eprintln!` statements** — `render.rs:1433,1455`
2. **Fix strict mode key mismatch** — `build_render_info` uses symbol keys, `render_liquid_template` reads string keys
3. **Re-run cucumber suite** to get current failure count

### Short-term

4. **Clean up dead code** — `NativeLinkTag` struct is unused (link resolution happens via `RubyTagRenderable`)
5. **Fix `include` with params in Rust** — Currently only simple `{% include filename.html %}` works natively; params fall through to Ruby
6. **Remove `include` from preprocessor `needs_raw`** and register proper `JekyllIncludeTag` — would simplify the flow
7. **Wire `slugify` as a Liquid filter** — Implementation exists in `slugify.rs` but not registered

### Medium-term (correctness)

8. **Fix `page.content` in for loops** (`rendering.feature:211`, `markdown.feature:6,20`)
9. **Fix `site.tags` iteration** (`site_data.feature:79,90`)
10. **Fix strict mode propagation** (`rendering.feature:66,85`) — likely related to the symbol/string key mismatch

### Long-term (performance)

11. **Include output caching** — Cache rendered includes keyed by context hash
12. **Native date filters** — Port `date_to_string` etc.
13. **Rust data model** (Phase 1 from RUST_OPTIMIZATION_PLAN.md)
14. **Parallel rendering** with rayon
