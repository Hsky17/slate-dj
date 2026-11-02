# DJ build — verified ground truth. Read this ENTIRELY before writing code.

You are adding an **in-app DJ** to the Slate music app: when the queue runs dry, the
app asks a local LLM (any OpenAI-compatible `chat/completions` endpoint, e.g. Ollama)
to pick the next genre, searches TIDAL for a block of tracks in that genre, and queues
them as the next autoplay block. Any failure falls back to the existing TRACK_MIX
radio autoplay, so this feature can NEVER wedge playback. Off by default.

All API facts below were VERIFIED against this checkout (grep anchors given). Trust the
code over this document if they disagree, and note the discrepancy in your summary.

## ABSOLUTE RULES (every run)
1. Do NOT add or change dependencies. No `cargo add`. Nothing in any `Cargo.toml`.
2. Do NOT modify: `tidal_api.rs`, `queue.rs`, anything under `mcp/`, any UI/Slint file,
   `startup.rs`, `control.rs`, `events.rs`.
3. Do NOT `git commit`, `git push`, or make tags.
4. Only create/modify the files listed for YOUR slice.
5. Definition of done is the exact command at the end of your slice. A failed or
   skipped test is a FAILURE — report it, never fake a pass.
6. Keep every function small. Match the codebase style (rustfmt defaults, `log::` for logging).
7. Use `#[serde(default)]` serde patterns exactly like neighbors.

## Verified facts (anchors)

- Workspace `slate-core` (lib) + `slate-app` (GUI backend). `reqwest 0.12` (features
  `json`, `socks`) is ALREADY a dependency of `slate-core` — `reqwest::Client::builder()...`,
  `.json(body)`, `.bearer_auth(key)` all available. `serde_json::json!`, `Value::pointer` available.
- `crates/slate-core/src/lib.rs:18` — `pub mod events;` (module decls live at top of lib.rs;
  `crate::AppHandle` is re-exported at lib.rs:25).
- `crates/slate-core/src/lib.rs:185` — `pub struct Settings { ... }` derives
  `#[derive(Debug, Serialize, Deserialize, Clone)]`; fields use `#[serde(default)]`;
  there is a manual `impl Default for Settings` after the struct (find it; if it is a
  struct literal add your field there too). `load_settings` is `pub async fn load_settings(&self) -> Option<Settings>` (near lib.rs:698).
- `crates/slate-core/src/controller.rs:434` — the ONLY call site you change in slice 2:
  `Next::NeedAutoplay => match self.autoplay(q).await {`
- `crates/slate-core/src/controller.rs:473` — `async fn autoplay(&self, q: &mut Queue) -> Option<Track>`:
  current track's `extra["mixes"]["TRACK_MIX"]` → `pages::get_mix_items(&self.state, mix_id)` →
  `Vec<TidalTrack>` → `Track::from_serialized` each → `q.install_autoplay(radio, |t| if prefs.blocks(t) { Some(Skip::Blocked) } else if t.is_unavailable(now_secs()) { Some(Skip::Unavailable) } else { None })`.
- `Controller` struct (controller.rs ~line 99): fields include `state: Arc<AppState>`,
  `queue: tokio::sync::Mutex<Queue>`, `prefs: std::sync::Mutex<ContentPrefs>`, `autoplay_once: AtomicBool`,
  constructed in `Controller::new` (struct literal — new fields need init there).
  Private method `async fn start(&self, q: &mut Queue, track: Track, chosen_by_user: bool) -> Result<(), PlayFailure>` (line 161).
  `Outcome` variants: `Playing(Track), Restarted, Stopped, GaveUp, Failed(PlayFailure), Nothing, WaitForAgent`.
  Module-level helper `fn now_secs() -> i64` exists in controller.rs.
- TIDAL client access (verified pattern from `commands/pages.rs:429`):
  `let mut client = state.tidal_client.lock().await; let res = client.search(&query, limit).await?; drop(client);`
  `TidalClient::search(&mut self, query: &str, limit: u32) -> Result<TidalSearchResults, SlateError>` (tidal_api.rs:4362).
  `TidalSearchResults { ... tracks: Vec<TidalTrack>, ... }` (tidal_api.rs:903); `TidalTrack` derives `Serialize` (tidal_api.rs:78).
- Queue API (queue.rs, all `pub`): `Track { id: u64, extra: Map<String, Value>, ... }` (line 13) with
  `artist_display()` (line 79) and `is_unavailable(now: i64) -> bool` (line 87);
  `Track::from_serialized<T: Serialize>(item: &T) -> Option<Track>` (line 66);
  `Queue { pub current: Option<Track> (line 310), pub history: Vec<Track> (line 314), ... }`;
  `Queue::install_autoplay(radio: Vec<Track>, skip: impl FnMut(&Track) -> Option<Skip>) -> Option<Track>` (line 749);
  `Skip::{Blocked, Unavailable}`.
- Events: `crate::events::AppHandle::from(&state).emit("event-name", payload).ok()` — verified usage at startup.rs:47 and controller's own `advance()` (`crate::events::AppHandle::from(&self.state).emit("autoplay-empty", ()).ok();`). `emit<S: Serialize>`.
- `slate-app/src/backend.rs:744` — `pub async fn advance(app: &AppHandle, explicit: bool) -> Outcome { app.state::<Controller>().next(explicit).await }` — the pattern for new backend fns. `Controller` and `Outcome` are already imported there.

---

## SLICE 1 (this run if you are run 1) — `crates/slate-core/src/dj.rs` + lib.rs wiring

### 1a. New file `crates/slate-core/src/dj.rs`
Write it EXACTLY like this (you may tidy imports/formatting; do not change behavior,
field names, function names, or doc-test expectations):

```rust
//! DJ: LLM-driven continuity across genre blocks.
//!
//! When the queue runs dry the controller asks a local LLM (any
//! OpenAI-compatible `/chat/completions` endpoint, e.g. Ollama) to pick the next
//! genre, searches TIDAL for a block of tracks in that style, and queues them as
//! the next autoplay block. Any failure falls back to the existing TRACK_MIX
//! radio autoplay, so this feature can never wedge playback.
//!
//! Off by default: `Settings.dj.enabled` (or `DJ_ENABLE=1`) turns it on.

use serde::{Deserialize, Serialize};

/// LLM DJ settings. All optional; `DJ_*` env vars override blanks at call time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct DjSettings {
    /// Master switch. Default: off.
    pub enabled: bool,
    /// OpenAI-compatible base URL, e.g. `http://127.0.0.1:11434/v1`.
    pub base_url: String,
    /// Bearer token. Blank = no Authorization header (local servers).
    pub api_key: String,
    /// Model name, e.g. `qwen3.8:latest`.
    pub model: String,
    /// Ask the LLM for a short spoken line for each block (display/TTS'd).
    pub speak: bool,
    /// Tracks per genre block at TIDAL search time. 0 = default (10).
    pub block_size: u32,
}

impl DjSettings {
    /// Apply `DJ_ENABLE`, `DJ_BASE_URL`, `DJ_MODEL`, `DJ_API_KEY`, `DJ_SPEAK`,
    /// `DJ_BLOCK` env-var overrides on top of settings-file values.
    pub fn resolved(&self) -> Self {
        let env = |k: &str| {
            std::env::var(k).ok().filter(|v| !v.trim().is_empty())
        };
        let mut r = self.clone();
        if let Some(v) = env("DJ_ENABLE") {
            r.enabled = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        if let Some(v) = env("DJ_BASE_URL") {
            r.base_url = v;
        }
        if let Some(v) = env("DJ_MODEL") {
            r.model = v;
        }
        if let Some(v) = env("DJ_API_KEY") {
            r.api_key = v;
        }
        if let Some(v) = env("DJ_SPEAK") {
            r.speak = matches!(v.trim(), "1" | "true" | "yes" | "on");
        }
        if let Some(v) = env("DJ_BLOCK").and_then(|v| v.trim().parse::<u32>().ok()) {
            r.block_size = v;
        }
        r
    }
}

/// One DJ decision: the genre block to queue next.
#[derive(Debug, Clone, Serialize)]
pub struct DjMove {
    pub query: String,
    pub reason: String,
    pub speaks: bool,
    pub line: Option<String>,
}

/// Parse the model's reply into a [`DjMove`]. Accepts plain JSON or JSON wrapped
/// in prose (outermost braces). `None` = unusable (caller falls back).
pub fn parse_move(raw: &str) -> Option<DjMove> {
    let text = raw.trim();
    let v = match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => v,
        Err(_) => {
            let start = text.find('{')?;
            let end = text.rfind('}')?;
            if end <= start {
                return None;
            }
            serde_json::from_str(&text[start..=end]).ok()?
        }
    };
    let query = v.get("query")?.as_str()?.trim().to_string();
    if query.is_empty() {
        return None;
    }
    Some(DjMove {
        query,
        reason: v
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        speaks: v
            .get("speaks")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        line: v
            .get("line")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
}

/// Snapshot of what is playing and what the listener asked for.
#[derive(Debug, Clone, Default)]
pub struct DjContext {
    /// (title, artist) of the current track, if any.
    pub current: Option<(String, String)>,
    /// Up to the last ~20 played tracks, oldest → newest, (title, artist).
    pub recent: Vec<(String, String)>,
    /// The listener's latest explicit direction, if any.
    pub direction: Option<String>,
    /// Why the current block was chosen (previous DJ reason).
    pub last_reason: Option<String>,
}

pub const SYSTEM_PROMPT: &str = "You are the DJ for a continuous music session. The listener asked you to keep the night going: when one genre block ends, choose the next genre or style so the set flows like one long DJ mix.
Rules:
- Keep the momentum: usually stay near the current genre; drift to adjacent styles, similar energy, or a smooth deliberate contrast.
- Vary the set: do not repeat the same query twice in a row and do not just ping-pong between two styles.
- If the listener gave a direction this turn, follow it; it outranks your own flow.
- Respond with ONLY this JSON: {\"query\": \"<2-6 word genre/style search phrase for the next block>\", \"reason\": \"<short clause: why it follows>\", \"speaks\": <true|false>, \"line\": \"<one short sentence to say to the listener; only when speaks is true and it helps>\"}.
- \"query\" must be a generic genre/style phrase (like \"deep house 124 bpm\" or \"late night soul\"), never a specific song or artist.";

pub fn build_user_prompt(ctx: &DjContext) -> String {
    let mut out = String::new();
    if let Some((t, a)) = &ctx.current {
        out.push_str(&format!("current: {t} - {a}\n"));
    }
    if let Some(r) = &ctx.last_reason {
        out.push_str(&format!("why this block: {r}\n"));
    }
    if !ctx.recent.is_empty() {
        out.push_str("recent set (oldest first):\n");
        for (t, a) in &ctx.recent {
            out.push_str(&format!("  {t} - {a}\n"));
        }
    }
    if let Some(d) = &ctx.direction {
        out.push_str(&format!("listener direction (follow it): {d}\n"));
    }
    out.push_str("Pick the next block now.");
    out
}

/// Best-effort spoken line via a system TTS if one is on PATH. Never blocks the
/// calling thread for long, never fails playback. Purely cosmetic.
pub fn speak(line: Option<&str>) {
    let Some(line) = line.map(str::trim).filter(|l| !l.is_empty()) else {
        return;
    };
    let Some(bin) = ["espeak-ng", "say"]
        .iter()
        .find(|b| {
            std::env::var_os("PATH")
                .and_then(|p| {
                    p.split(std::path::MainSeparator as _)
                        .map(|d| std::path::Path::new(d).join(b))
                        .find(|c| c.is_file())
                })
                .is_some()
        })
    else {
        log::info!("[dj] would say: {line}");
        return;
    };
    std::thread::spawn(move || {
        if *bin == "say" {
            let _ = std::process::Command::new(bin).arg(line).output();
        } else {
            let _ = std::process::Command::new(bin).args(["--text", line]).output();
        }
    });
}

/// Client for one OpenAI-compatible chat endpoint.
pub struct Dj {
    http: reqwest::Client,
    cfg: DjSettings,
}

impl Dj {
    pub fn new(settings: DjSettings) -> Self {
        let cfg = settings.resolved();
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(45))
            .build()
            .unwrap_or_default();
        Self { http, cfg }
    }

    pub fn enabled(&self) -> bool {
        self.cfg.enabled
    }

    pub fn block_size(&self) -> u32 {
        if self.cfg.block_size == 0 {
            10
        } else {
            self.cfg.block_size
        }
    }

    /// Ask the LLM for the next block. `None` = unavailable or unparseable; the
    /// caller falls back to the usual autoplay radio.
    pub async fn decide(&self, ctx: &DjContext) -> Option<DjMove> {
        let base_url = self.cfg.base_url.trim();
        if base_url.is_empty() || self.cfg.model.trim().is_empty() {
            return None;
        }
        let url = format!("{base_url}/chat/completions", base_url = base_url.trim_end_matches('/'));
        let user = build_user_prompt(ctx);
        let base = serde_json::json!({
            "model": self.cfg.model,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": user },
            ],
            "temperature": 0.9,
        });

        // 1) with an OpenAI-style JSON schema (Ollama accepts this too).
        let mut body = base.clone();
        body["response_format"] = serde_json::json!({
            "type": "json_schema",
            "json_schema": {
                "name": "dj_move",
                "strict": false,
                "schema": {
                    "type": "object",
                    "properties": {
                        "query":  { "type": "string" },
                        "reason": { "type": "string" },
                        "speaks": { "type": "boolean" },
                        "line":   { "type": "string" },
                    },
                    "required": ["query"],
                },
            },
        });
        if let Ok(raw) = self.post(&url, &body).await {
            return parse_move(&raw);
        }
        // 2) retry plain, then parse whatever came back.
        self.post(&url, &base).await.ok().and_then(|raw| parse_move(&raw))
    }

    async fn post(&self, url: &str, body: &serde_json::Value) -> Result<String, String> {
        let mut req = self.http.post(url).json(body);
        if !self.cfg.api_key.trim().is_empty() {
            req = req.bearer_auth(self.cfg.api_key.trim());
        }
        let resp = req.send().await.map_err(|e| e.to_string())?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| e.to_string())?;
        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status.as_u16(), &text[..text.len().min(200)]));
        }
        let v: serde_json::Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let content = v
            .pointer("/choices/0/message/content")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "no choices[0].message.content in response".to_string())?;
        Ok(content.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plain_json() {
        let m = parse_move(r#"{"query":"deep house","reason":"keeps the groove","speaks":true,"line":"Deep from here."}"#).unwrap();
        assert_eq!(m.query, "deep house");
        assert_eq!(m.reason, "keeps the groove");
        assert!(m.speaks);
        assert_eq!(m.line.as_deref(), Some("Deep from here."));
    }

    #[test]
    fn parse_json_wrapped_in_prose() {
        let m = parse_move("Sure thing!\n{\"query\":\"jazz fusion\",\"reason\":\"drift\"}\nEnjoy.")
            .unwrap();
        assert_eq!(m.query, "jazz fusion");
        assert!(!m.speaks);
        assert!(m.line.is_none());
    }

    #[test]
    fn parse_rejects_empty_query_and_garbage() {
        assert!(parse_move(r#"{"query":"   "}"#).is_none());
        assert!(parse_move("no json here at all").is_none());
        assert!(parse_move(r#"{"reason":"no query field"}"#).is_none());
        assert!(parse_move("").is_none());
    }

    #[test]
    fn prompt_covers_direction_recent_and_current() {
        let p = build_user_prompt(&DjContext {
            current: Some(("Song".into(), "Artist".into())),
            recent: vec![("A".into(), "B".into()), ("C".into(), "D".into())],
            direction: Some("more jazz".into()),
            last_reason: Some("flow".into()),
        });
        assert!(p.contains("Song - Artist"));
        assert!(p.contains("A - B"));
        assert!(p.contains("C - D"));
        assert!(p.contains("more jazz"));
        assert!(p.contains("flow"));
        assert!(p.contains("Pick the next block now."));
    }

    #[test]
    fn default_settings_are_off() {
        let s = DjSettings::default();
        assert!(!s.enabled);
        assert!(s.base_url.is_empty());
        assert_eq!(Dj::new(s).block_size(), 10);
    }
}
```

NOTE on the `speak` PATH-split code above: if `p.split(std::path::MainSeparator as _)` does not
compile on this platform (it's a Windows-ism), replace that block with
`p.split(':').map(|d| std::path::Path::new(d).join(b)).find(|c| c.is_file())` — a colon
separator is correct on Linux. Make it compile; keep behavior (find espeak-ng, then say).

### 1b. `crates/slate-core/src/lib.rs` — two insertions
1. In the module declarations block (near `pub mod events;` at line 18): add `pub mod dj;`
2. Inside `pub struct Settings { ... }` (line 185, insert anywhere among the fields, e.g. right
   after `pub crossfade_secs: u32,` at line 220):
   ```rust
    /// DJ: LLM-driven genre continuity (off by default).
    #[serde(default)]
   pub dj: crate::dj::DjSettings,
   ```
3. Check the manual `impl Default for Settings` (right after the struct): if it is a struct
   literal, add `dj: crate::dj::DjSettings::default(),`. If it delegates (e.g.
   `..Default::default()`), nothing further is needed.

### SLICE 1 DO-DONE
`cargo test -p slate-core dj`
— must compile and pass all `dj::tests::*`. `cargo check -p slate-core` must be clean
(no new warnings from our files).

---

## SLICE 2 (next run) — controller integration + backend surface

Assume slice 1 landed. Changes EXACTLY:

### 2a. `crates/slate-core/src/controller.rs`
1. In `pub struct Controller { ... }` add two fields:
   ```rust
    /// Listener's latest explicit direction (consumed by the next DJ pick).
    dj_direction: std::sync::Mutex<Option<String>>,
    /// Why the current DJ block was chosen (fed back into the next prompt).
    dj_reason: std::sync::Mutex<Option<String>>,
   ```
   and initialize them in `Controller::new`'s struct literal:
   ```rust
    dj_direction: std::sync::Mutex::new(None),
    dj_reason: std::sync::Mutex::new(None),
   ```
2. After `async fn autoplay(...)` (line ~473-497) add these methods:
   ```rust
    /// Set the listener's next DJ direction (consumed by the next genre pick).
    pub fn dj_set_direction(&self, text: String) {
        if let Ok(mut g) = self.dj_direction.lock() {
            *g = Some(text);
        }
    }

    fn dj_take_direction(&self) -> Option<String> {
        self.dj_direction.lock().ok().and_then(|mut g| g.take())
    }

    fn dj_last_reason(&self) -> Option<String> {
        self.dj_reason.lock().ok().and_then(|g| g.clone())
    }

    /// DJ autoplay: LLM picks the next genre, TIDAL search fills a block, install it.
    /// `None` = DJ unavailable/disabled → caller falls back to `autoplay`.
    async fn dj_autoplay(&self, q: &mut Queue) -> Option<Track> {
        let settings = self.state.load_settings().await?;
        if !settings.dj.enabled {
            return None;
        }
        let dj = crate::dj::Dj::new(settings.dj);

        let title = |t: &Track| t.extra
            .get("title")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let artist = |t: &Track| t.artist_display();
        let ctx = crate::dj::DjContext {
            current: q.current.as_ref().map(|t| (title(t), artist(t).to_string())),
            recent: q
                .history
                .iter()
                .rev()
                .take(20)
                .map(|t| (title(t), artist(t).to_string()))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            direction: self.dj_take_direction(),
            last_reason: self.dj_last_reason(),
        };
        let move_ = dj.decide(&ctx).await?;

        let mut client = self.state.tidal_client.lock().await;
        let results = client.search(&move_.query, dj.block_size()).await.ok()?;
        drop(client);
        let radio: Vec<Track> = results
            .tracks
            .iter()
            .filter_map(Track::from_serialized)
            .collect();
        if radio.is_empty() {
            return None;
        }

        let prefs = self.prefs();
        let first = q.install_autoplay(radio, |t| {
            if prefs.blocks(t) {
                Some(Skip::Blocked)
            } else if t.is_unavailable(now_secs()) {
                Some(Skip::Unavailable)
            } else {
                None
            }
        })?;

        if !move_.reason.is_empty() {
            if let Ok(mut g) = self.dj_reason.lock() {
                *g = Some(move_.reason.clone());
            }
        }
        crate::events::AppHandle::from(&self.state)
            .emit(
                "dj-branch",
                serde_json::json!({
                    "query": move_.query,
                    "reason": move_.reason,
                    "speaks": move_.speaks,
                    "line": move_.line,
                }),
            )
            .ok();
        if move_.speaks {
            crate::dj::speak(move_.line.as_deref());
        }
        Some(first)
    }

    /// DJ if available, else the usual TRACK_MIX radio. Drop-in for `autoplay`.
    async fn autoplay_or_dj(&self, q: &mut Queue) -> Option<Track> {
        if let Some(first) = self.dj_autoplay(q).await {
            return Some(first);
        }
        self.autoplay(q).await
    }

    /// Force the DJ to pick a new block right now (manual trigger).
    pub async fn dj_next(&self) -> Outcome {
        let mut q = self.queue.lock().await;
        let Some(first) = self.dj_autoplay(&mut q).await else {
            return Outcome::Nothing;
        };
        match self.start(&mut q, first.clone(), true).await {
            Ok(()) => Outcome::Playing(first),
            Err(f) => Outcome::Failed(f),
        }
    }
   ```
   (Adjust types if `artist_display()` returns `Arc<str>` etc. — keep `.to_string()` where needed.)
3. THE ONE-LINE call-site change at controller.rs:434:
   `Next::NeedAutoplay => match self.autoplay(q).await {`
   becomes
   `Next::NeedAutoplay => match self.autoplay_or_dj(q).await {`

### 2b. `crates/slate-app/src/backend.rs` — right after `advance` (line ~744):
   ```rust
   /// DJ: force a new genre block right now (manual trigger).
   pub async fn dj_next(app: &AppHandle) -> Outcome {
    app.state::<Controller>().dj_next().await
   }

   /// Set the listener's next DJ direction (consumed on the next genre pick).
   pub fn dj_direction(app: &AppHandle, text: String) {
    app.state::<Controller>().dj_set_direction(text);
   }
   ```

### SLICE 2 DO-DONE
1. `cargo build -p slate-app` — must succeed.
2. `cargo test -p slate-core` — full suite must stay green (long: ~10+ min; let it run).
Do NOT "optimize" surrounding code. Do NOT touch the `autoplay_once`/`WaitForAgent` arms.

## LATER — DJ timeout (DONE, do not redo)
The endpoint test used to fail with a bare transport error ("error sending request
for url (…)") because `post()` capped every request at 45 s — too short for a local
model loading + generating. Now: `DjSettings.timeout_secs` (0 = default 300, clamp
1..=3600), env `DJ_TIMEOUT`, `Dj::timeout_secs()`, `Dj::send_error()` classifies
connect vs timeout for human messages, Settings → DJ mixer has a "DJ timeout"
Segmented (1/2/5/10/15 min, `dj-timeout` → `DJ_TIMEOUT_SECS` in `settings.rs`),
and `settings.rs` maps the picker back on `apply()`. Tests:
`effective_timeout_is_generous_and_clamped`,
`ping_names_the_timeout_when_the_model_is_slow`,
`ping_names_an_unreachable_server`, opt-in `ping_live` (#[ignore]).
