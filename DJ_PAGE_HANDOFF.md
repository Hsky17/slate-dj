# Handoff: DJ as its own category + DJ stage page (proof of concept)

Session handoff — read this top to bottom, then follow the Build plan.
Working dir: `~/Downloads/GitClones/slate-dj` (branch `feat/dj-mixer`).
Companion docs: `DJ_OPENCODE_PROMPT.md` (spec), `DJ_BUILD_NOTES.md` (cheat-sheet, 575 lines), absolute rules at the top of both.

## Where things stand (verified, not claimed)

DJ mode works end to end and is committed:

- `7635be4` LLM genre-decision module: `DjSettings`, prompt, OpenAI-compatible client (`Dj`), `parse_move`, system-TTS hook `speak()`.
- `149afe2` controller autoplay intervention + manual "DJ up a block" trigger.
- `56267f2` settings UI + persistence + docs.
- `54ec157` endpoint **Test button**, Connection status indicator, and no more silent failures (every `dj_autoplay` branch records a human-readable reason; the manual button surfaces it in the status bar).
- `dj-api-key` hint added (this session): field left blank works in code — the only required fields are base URL + model (`dj.rs:241/313`); the bearer header is attached only when non-empty (`dj.rs:339`). The hint explains that *some endpoints* reject a request with no Authorization header at all, in which case any filler key satisfies them.

Verified state at handoff:

- `cargo check --workspace --all-targets` → 0 errors.
- `cargo test -p slate-core` → lib 558 passed + all integration suites, 0 failed.
- `cargo test -p slate` → 150 passed, 2 ignored, 0 failed. (The app crate package name is `slate`, NOT `slate-app`.)
- Debug binary exists at `target/debug/slate` (~1.2 GB — see Binary size).

User-accepted behavior to preserve: DJ picks the next genre block from the current queue + direction; TIDAL search fills the queue as an autoplay install; on any failure the radio fallback plays and the status bar/Connection row says *why*.

## Binary size — why it looks much bigger (measure first, don't guess)

The "original release" you compared against is 69 MB (`~/Downloads/GitClones/slate/target/release/slate`). The slate-dj `target/debug/slate` is ~1.2 GB. That is almost entirely a **profile difference**, not DJ code:

- Debug: `-g` debuginfo on every crate, no optimization, unstripped.
- Release: `Cargo.toml [profile.release]` already has `lto = "thin"`, `codegen-units = 1`, `strip = true`.

**Do this first** in the new session:

```sh
cd ~/Downloads/GitClones/slate-dj
cargo build --release
ls -lh target/release/slate target/debug/slate
du -sh target/release/slate ~/.cache 2>/dev/null
```

(As of this handoff a release build of slate-dj was still compiling; expect something close to the 69 MB original — measure the actual delta and record it.)

If the release binary is meaningfully larger than 69 MB, the usual suspects, in order:

1. `cargo build --release --profile release-min` with a new profile in `Cargo.toml`:
   ```toml
   [profile.release-min]
   inherits = "release"
   opt-level = "z"        # or "s"
   lto = "fat"            # "thin" is fine if fat is too slow
   strip = "symbols"
   ```
   (Only add if the delta justifies the slower build.)
2. `cargo bloat --release -n 40` (via `cargo install cargo-bloat` in a scratch dir if not installed) to see where bytes went — compare against a bloat of `~/Downloads/GitClones/slate`.
3. Confirm no new dependencies slipped in: the DJ work added **zero** (absolute rule). `diff <(grep 'name = ' Cargo.lock | sort) <(grep 'name = ' ~/Downloads/GitClones/slate/Cargo.lock | sort)` should show only version drift, not new crates.
4. Slint/shaper: check whether the Slint feature set changed (e.g. `femtovg`) between the two repos' `crates/slate-app/Cargo.toml` — a feature delta can move MBs.

Do not "fix" the debug size — 1.2 GB debug is normal and only matters for shipping a release.

## The feature (proof of concept, visual)

Goal: pull DJ out of the "settings only" mental model into the app itself:

1. **A "DJ" category in the nav rail** (next to Home/Albums/Artists/Mixes/Explore in `crates/slate-app/ui/library.slint:201-214`, the `for item in [...]` NavItem list). Visible when DJ is enabled (`SettingsState.dj-enabled`); selecting it **activates the mode** — i.e. it both shows the DJ page and ensures `dj-enabled` is on (call the existing `dj-test`-style action path or the existing `SettingsState.action("dj-toggle"/...)` — read the `dj-` dispatch in `crates/slate-app/src/settings.rs` before inventing a new action).
2. **A dedicated DJ page** (new `view` value alongside `root.view == "cards"` at `crates/slate-app/ui/app.slint:460`):
   - **Queue view** for the DJ — reuse `QueuePanel` (`crates/slate-app/ui/pages.slint:177`), which already renders `<[QueueRow]>`, now playing, play/remove/clear. The app already carries `in property <[QueueRow]> queue-rows` (`app.slint:222`) and feeds panels from it (`app.slint:500`). So the DJ page can embed the same `queue-rows` source without new data plumbing.
   - **TTS for transitions** — the machinery exists: `DjMove { speaks: bool, line: String }` (`crates/slate-core/src/dj.rs:64`), `Dj` already asks the model for a spoken line per block, and `controller.rs:653-659` already calls `crate::dj::speak(move_.line.as_deref())` when `move_.speaks`. `speak()` (`dj.rs:150-172`) shells out to `espeak-ng` or `say` on PATH on a spawned thread; it never fails playback. For the PoC:
     - Show the `line` on the DJ page as the current transition ("Now playing: …", spoken by the model) — surface it in a `QueueRow`-adjacent property or a `Text` bound to a new `in property <string> dj-last-line`.
     - Check the user's PATH has `espeak-ng` (`which espeak-ng || which say`); if not, the page should say "TTS unavailable on this machine" rather than pretend.
     - Keep the system-TTS approach for the PoC; do NOT add a new TTS dependency (absolute rule: no new deps).
   - **Fun facts about the genre it's playing** — no data source exists yet. PoC: a small static `GenreTrivia` lookup in a new file (e.g. `crates/slate-core/src/dj_trivia.rs`) keyed on common genres (house, techno, deep house, afro-house, ambient, lo-fi, jazz, disco, drum & bass, UK garage, etc.), 1–3 short facts each, with a graceful "Unknown genre — ask me the direction" fallback (the model already returns a `query` phrase; reuse it as the displayed genre). No network, no new deps. If you want it dynamic, `Dj` can be asked for a fact in `decide()`'s JSON already (it has a `reason` field) — reuse `reason` first, static trivia second.
   - **Layout**: middle-aligned, Slate theme. Theme tokens are in `crates/slate-app/ui/theme.slint` (`Theme.accent`, `Theme.surface`, `Theme.text`, `Theme.text-muted`, `Theme.background`, `Theme.surface-raised`). The rail is dark with green `Theme.accent` (screenshot: "Slate" title, nav items, PLAYLISTS section). Center content in a `VerticalLayout` with `vertical-stretch` and `Text { text-alignment: center }`; keep it intentionally plain — the user explicitly said "doesn't have to be anything crazy looking right now, more just a proof of concept".
3. **Activating the mode from the category**: selecting DJ should turn `dj-enabled` on and persist it through the existing settings-surface (`settings.rs` `dj-` dispatch / `slate_core::dj::DjSettings`). Reading the current `dj-` action handlers first is mandatory before touching `utility.rs`.

## Anchor index for this feature (verified in this session)

| Concern | File:line |
|---|---|
| Nav item list (add DJ here) | `crates/slate-app/ui/library.slint:201-214` |
| Nav `go(id)` / `selected` | `crates/slate-app/ui/library.slint:173-176` |
| Main view swap (`view == "cards"`) | `crates/slate-app/ui/app.slint:460` |
| Reusable queue panel | `crates/slate-app/ui/pages.slint:177` (`QueuePanel`) |
 | `app.slint:222` (`queue-rows`), `app.slint:500` (panel feed), `types.slint` (`QueueRow`) |
| TTS hook (exists, system) | `crates/slate-core/src/dj.rs:150-172` (`speak()`), `dj.rs:64` (`DjMove.speaks/line`) |
| TTS fired on autoplay | `crates/slate-core/src/controller.rs:653-659` |
| DJ JSON contract (query/reason/speaks/line) | `crates/slate-core/src/dj.rs:126` (prompt), `dj.rs:26` (`speak: bool` setting) |
| Settings UI for DJ (keys, test row) | `crates/slate-app/ui/settings.slint:525-547` |
| `dj-` action dispatch in Rust | `crates/slate-app/src/settings.rs` (search `dj-`) |
| Endpoint test + status | `crates/slate-core/src/commands/utility.rs` (`dj_test_endpoint`), `controller.rs` (`dj_status`), `backend.rs` (`dj_next` error surfacing) |
| App backend queue refresh | `crates/slate-app/src/backend.rs:722` (`refresh_queue_info`) |
| Theme tokens | `crates/slate-app/ui/theme.slint` |
| Absolute rules | `DJ_BUILD_NOTES.md` + `DJ_OPENCODE_PROMPT.md` (top of file) |

## Build plan (suggested, each a small verified slice)

1. `dj-enabled` + nav item + view swap stub → DJ page renders (empty queue panel + "DJ is on" + disabled state text). Verify with `cargo check -p slate` + a manual run.
2. Wire real `queue-rows` into the DJ page (reuse existing plumbing). Verify queue stays in sync with the library view.
3. Surface the model's transition `line` (`speaks`) + a TTS availability check. Verify `espeak-ng`/`say` presence; confirm `speak` is already called on autoplay.
4. Static genre trivia + graceful fallback. Verify a handful of known genres + one unknown falls back to the model `reason`/`query`.
5. (Stretch) "Direction" input on the DJ page (reuse `dj-direction`/`dj-go` already in settings).

Each slice: build → test the touched crate → commit with a specific message. Do not batch.

## Verification protocol (unchanged, non-negotiable)

- `cargo check --workspace --all-targets` → 0 errors.
- `cargo test -p slate-core` → 0 failed (lib 558 at handoff; grow only with new unit tests, never regress).
- `cargo test -p slate` → 0 failed (150 at handoff).
- Run the debug binary: `./target/debug/slate` and confirm:
  - DJ nav item appears when enabled, mode activates on select.
  - DJ page shows the live queue, a transition line, and a fun fact (or fallback) for the current genre.
  - TTS actually reads the transition line aloud (or the page says TTS unavailable).
  - No regression to the library/queue panel in the main view.
- Commit per slice; never `git push`; no new deps; don't modify `tidal_api.rs`, `queue.rs`, `mcp/`, `startup.rs`, `control.rs`, `events.rs` (per absolute rules).

## Absolute rules (restated)

- No new dependencies (this includes TTS — keep system-TTS via existing `speak()`).
- All HTTP through `ProxiedHttp`; tests use `ProxiedHttp::blocked("…")`; satisfy `tests/source_guards.rs`.
- Do **not** log, print, or persist the live DJ API key — any live value = [REDACTED].
- Keep slices pure and small; failed test = FAILURE, not a note.
- The user prefers working over pretty: ship a plain, middle-aligned, on-theme PoC and leave cosmetics for later.

## Environment / gotchas

- App crate package name is `slate` (not `slate-app`. Use `cargo check/build/test -p slate`).
- Wiremock API: `Mock::given(...).mount(&server).await` (0.6.5), NOT `.to(&server)` (this bit a prior session).
- `mss`/ImageMagick screen captures of Tk windows come back BLACK on this compositor even when the UI is fine — black capture ≠ broken UI; on-screen truth wins (not directly blocking here, but relevant if you screenshot the debug binary).
- `cargo test --workspace` has hit a 30-min external SIGTERM in the past; individual crates (`-p slate-core`, `-p slate`) are the reliable signal.
- DJ endpoint config: base URL + model are the only hard requirements; API key optional in code. Env overrides `DJ_BASE_URL`, `DJ_MODEL`, `DJ_API_KEY`. The endpoint is `{base}/chat/completions` (45s timeout) — an OpenAI-compatible chat endpoint.
