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
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
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
    let Some(bin) = ["espeak-ng", "say"].into_iter().find(|b| {
        std::env::var_os("PATH").and_then(|p| {
            p.to_str()?.split(std::path::MAIN_SEPARATOR).map(|d| {
                std::path::Path::new(d).join(*b)
            }).find(|c| c.is_file())
        }).is_some()
    }) else {
        log::info!("[dj] would say: {line}");
        return;
    };
    let line = line.to_string();
    std::thread::spawn(move || {
        if bin == "say" {
            let _ = std::process::Command::new(bin).arg(&line).output();
        } else {
            let _ = std::process::Command::new(bin).args(["--text", line.as_str()]).output();
        }
    });
}

/// Client for one OpenAI-compatible chat endpoint. Never builds its own
/// connection: the app hands over its shared proxy-aware `ProxiedHttp`, so a
/// blocked proxy degrades the DJ to the radio fallback instead of egressing
/// direct (the rule `tests/source_guards.rs` enforces).
pub struct Dj {
    http: crate::proxy_http::ProxiedHttp,
    cfg: DjSettings,
}

impl Dj {
    pub fn new(
        settings: DjSettings,
        http: crate::proxy_http::ProxiedHttp,
    ) -> Self {
        Self {
            cfg: settings.resolved(),
            http,
        }
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
        let client = self.http.client().map_err(|e| e.cause)?;
        let mut req = client
            .post(url)
            .json(body)
            .timeout(std::time::Duration::from_secs(45));
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
        let m =
            parse_move(r#"{"query":"deep house","reason":"keeps the groove","speaks":true,"line":"Deep from here."}"#).unwrap();
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
        assert_eq!(
            Dj::new(s, crate::proxy_http::ProxiedHttp::blocked("test")).block_size(),
            10
        );
    }
}
