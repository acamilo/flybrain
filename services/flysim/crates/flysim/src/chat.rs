//! The chat path: the sanitizer, the operator deny list, the admission limits and the ring.
//!
//! Binding contract: `docs/control-api.md`, `POST /chat` and the `[chat]` config block. The
//! sanitizer is the Rust half of a rule set written twice on purpose — `packages/feed/src/chat.ts`
//! is the other half, and `packages/feed/tests/fixtures/chat-cases.json` is loaded by both test
//! suites (`tests/chat.rs` here) so neither copy can drift.
//!
//! The rules, in the order they are applied — the order is part of the contract, because the
//! reason is a metric label (`fly_chat_rejected_total{reason}`):
//!
//!  1. NFC-normalize, then turn tab/CR/LF into spaces.
//!  2. `control`: any remaining `Cc` code point refuses the line.
//!  3. `charset`: every code point must be alphabetic, numeric, a space (any `White_Space` code
//!     point is folded to one first) or one of [`ALLOWED_PUNCTUATION`] /
//!     [`ALLOWED_EXTRA_PUNCTUATION`]. This is what refuses emoji, combining marks (zalgo),
//!     zero-width characters, bidi overrides and the BOM.
//!  4. Collapse runs of spaces and trim. `empty`: nothing left.
//!  5. `too_long`: more than [`MAX_TEXT_LENGTH`] code points.
//!  6. `url`: `://`, `www.`, or a TLD-like token. Deliberately over-eager: `Mr.Mime` is refused,
//!     `e.g.` and `3.14` are not.
//!
//! `char::is_alphabetic` and `char::is_numeric` are the Alphabetic and Numeric Unicode properties,
//! which is exactly what `\p{Alphabetic}` and `\p{Number}` mean in the TypeScript regex.
//!
//! Chat never reaches the simulation. Nothing in this module can touch the network, the emulator
//! or the reward path; the sim thread appends the line to a ring and the event log, and that is
//! the whole effect.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::ratelimit::RateLimiter;
use crate::snapshot::ChatLine;

/// Longest accepted line, in Unicode code points.
pub const MAX_TEXT_LENGTH: usize = 200;

/// Hard ceiling on `[chat] ring`, and on the `chat` array in `packages/feed/src/schema.json`.
pub const RING_MAX: usize = 12;

/// One accepted line per name per this many milliseconds (`docs/control-api.md`).
pub const PER_NAME_MS: u64 = 2_000;

/// Accepted lines per second across every name.
pub const PER_SECOND: usize = 5;

/// Longest accepted display name, in code points (`packages/feed/src/names.ts`).
pub const MAX_NAME_LENGTH: usize = 25;

/// The ASCII punctuation a chat line may contain, besides letters, digits and spaces. Same set as
/// `ALLOWED_PUNCTUATION` in `packages/feed/src/chat.ts`.
pub const ALLOWED_PUNCTUATION: &str = "!\"#%&'()*+,-./:;=?@[]_{}~";

/// Non-ASCII punctuation a chat line may also contain: the typographic marks phone keyboards
/// produce by themselves, and the CJK equivalents of the ASCII stops. An explicit list rather than
/// the Unicode `P*` categories, because the standard library has no `is_punctuation` and this set
/// has to be identical to `ALLOWED_EXTRA_PUNCTUATION` in `packages/feed/src/chat.ts` without
/// either side taking a dependency. Symbols stay out, so emoji are still refused.
pub const ALLOWED_EXTRA_PUNCTUATION: &str =
    "\u{2013}\u{2014}\u{2018}\u{2019}\u{201c}\u{201d}\u{2026}\u{a1}\u{bf}\u{b7}\u{3001}\u{3002}\u{300c}\u{300d}\u{ff01}\u{ff1f}";

/// Why a chat line was refused. Also the value of the `reason` label on
/// `fly_chat_rejected_total`, so the set is closed and its spellings are the contract's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// The request body was not `{ by, text, bot? }`.
    Malformed,
    /// A `Cc` code point.
    Control,
    /// A code point outside the allowed set: emoji, combining marks, zero-width, bidi, BOM.
    Charset,
    /// Nothing left after collapsing whitespace.
    Empty,
    /// More than [`MAX_TEXT_LENGTH`] code points.
    TooLong,
    /// A scheme, a `www.`, or a TLD-like token.
    Url,
    /// The display name is not `letters/digits/underscore, 1..=25`.
    Name,
    /// An operator deny-list pattern matched the name or the text.
    DenyList,
    /// The per-name or global limit refused it (the response is a 429, not a 422).
    RateLimited,
}

impl RejectReason {
    /// Every reason, so `/metrics` can publish a zero series for each one rather than making a
    /// dashboard wait for the first refusal to learn the label exists.
    pub const ALL: [Self; 9] = [
        Self::Malformed,
        Self::Control,
        Self::Charset,
        Self::Empty,
        Self::TooLong,
        Self::Url,
        Self::Name,
        Self::DenyList,
        Self::RateLimited,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Malformed => "malformed",
            Self::Control => "control",
            Self::Charset => "charset",
            Self::Empty => "empty",
            Self::TooLong => "too_long",
            Self::Url => "url",
            Self::Name => "name",
            Self::DenyList => "deny_list",
            Self::RateLimited => "rate_limited",
        }
    }

    /// Index into the per-reason counter array in [`crate::metrics::Metrics`].
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|reason| *reason == self)
            .expect("ALL contains every reason")
    }
}

/// Sanitize one chat message, or say which rule refused it.
///
/// Whole-line: a message either survives every rule or is dropped. Never panics, and is
/// idempotent — the page and the service both re-run it as defence in depth.
pub fn sanitize_chat_text(text: &str) -> Result<String, RejectReason> {
    let normalized: String = text
        .nfc()
        .map(|character| match character {
            '\t' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    if normalized.chars().any(|character| character.is_control()) {
        return Err(RejectReason::Control);
    }

    let spaced: String = normalized
        .chars()
        .map(|character| if character.is_whitespace() { ' ' } else { character })
        .collect();
    if !spaced.chars().all(is_allowed) {
        return Err(RejectReason::Charset);
    }

    let collapsed = collapse_spaces(&spaced);
    if collapsed.is_empty() {
        return Err(RejectReason::Empty);
    }
    if collapsed.chars().count() > MAX_TEXT_LENGTH {
        return Err(RejectReason::TooLong);
    }
    if looks_like_url(&collapsed) {
        return Err(RejectReason::Url);
    }
    Ok(collapsed)
}

/// `^[\p{L}\p{N}_]{1,25}$`, the rule `packages/feed/src/names.ts` applies before the bridge ever
/// calls us. Enforced again here because `docs/control-api.md` says limits hold "regardless of
/// what the bridge does".
pub fn is_valid_display_name(name: &str) -> bool {
    let length = name.chars().count();
    (1..=MAX_NAME_LENGTH).contains(&length)
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_')
}

fn is_allowed(character: char) -> bool {
    character == ' '
        || character.is_alphabetic()
        || character.is_numeric()
        || ALLOWED_PUNCTUATION.contains(character)
        || ALLOWED_EXTRA_PUNCTUATION.contains(character)
}

fn collapse_spaces(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_space = true; // leading spaces are dropped
    for character in text.chars() {
        if character == ' ' {
            if !previous_space {
                out.push(' ');
            }
            previous_space = true;
        } else {
            out.push(character);
            previous_space = false;
        }
    }
    while out.ends_with(' ') {
        out.pop();
    }
    out
}

/// Whether a sanitized line advertises a link.
///
/// ASCII-only lowercasing on purpose: `str::to_lowercase` and JavaScript's `toLowerCase()`
/// disagree about a handful of non-ASCII code points (and one of them changes length), and only
/// ASCII matters for a host name.
pub fn looks_like_url(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    if lower.contains("://") || lower.contains("www.") {
        return true;
    }
    for token in lower.split(' ') {
        let characters: Vec<char> = token.chars().collect();
        if characters.len() < 2 {
            continue;
        }
        for index in 1..characters.len() - 1 {
            if characters[index] != '.' {
                continue;
            }
            if !characters[index - 1].is_ascii_alphanumeric() {
                continue;
            }
            let letters = characters[index + 1..]
                .iter()
                .take_while(|character| character.is_ascii_lowercase())
                .count();
            if letters >= 2 {
                return true;
            }
        }
    }
    false
}

// -- the operator deny list -------------------------------------------------------------------

/// `[chat] deny_list`: one pattern per line, `#` comments, matched case-insensitively anywhere in
/// the display name or the sanitized text.
///
/// Reloaded on SIGHUP and, so an operator who forgets the signal is not surprised, at most every
/// [`DenyList::RELOAD_INTERVAL`] anyway. A missing or unreadable file is an empty list and a
/// warning, never a startup failure: the stream matters more than the filter, and the sanitizer
/// plus AutoMod are still in front of it.
#[derive(Debug, Clone)]
pub struct DenyList {
    path: Option<PathBuf>,
    patterns: Vec<String>,
    next_check: Instant,
}

impl DenyList {
    /// How often the file is re-read without a SIGHUP.
    pub const RELOAD_INTERVAL: Duration = Duration::from_secs(60);

    /// Read the file now. `None` (no configured path) is an always-empty list.
    pub fn load(path: Option<&Path>, now: Instant) -> Self {
        let mut list =
            Self { path: path.map(Path::to_path_buf), patterns: Vec::new(), next_check: now };
        list.reload();
        list.next_check = now + Self::RELOAD_INTERVAL;
        list
    }

    /// An empty list with no file behind it, for tests and for `enabled = false`.
    pub fn empty(now: Instant) -> Self {
        Self { path: None, patterns: Vec::new(), next_check: now + Self::RELOAD_INTERVAL }
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Re-read the file if SIGHUP asked (`force`) or the interval has elapsed. Returns whether it
    /// read the file.
    pub fn maybe_reload(&mut self, now: Instant, force: bool) -> bool {
        if !force && now < self.next_check {
            return false;
        }
        self.next_check = now + Self::RELOAD_INTERVAL;
        let before = self.patterns.len();
        self.reload();
        if force || before != self.patterns.len() {
            tracing::info!(
                patterns = self.patterns.len(),
                path = ?self.path,
                forced = force,
                "chat deny list reloaded"
            );
        }
        true
    }

    /// Whether a pattern matches the name or the sanitized text.
    pub fn blocks(&self, by: &str, text: &str) -> bool {
        if self.patterns.is_empty() {
            return false;
        }
        let name = by.to_lowercase();
        let body = text.to_lowercase();
        self.patterns
            .iter()
            .any(|pattern| body.contains(pattern) || name.contains(pattern))
    }

    fn reload(&mut self) {
        let Some(path) = self.path.as_deref() else {
            self.patterns.clear();
            return;
        };
        match std::fs::read_to_string(path) {
            Ok(text) => self.patterns = parse_deny_list(&text),
            Err(error) => {
                if !self.patterns.is_empty() {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "could not re-read the chat deny list; keeping the patterns already loaded"
                    );
                } else {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "no chat deny list; the sanitizer and AutoMod are the only filters"
                    );
                }
            }
        }
    }
}

/// One lowercased pattern per non-blank, non-comment line.
pub fn parse_deny_list(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.to_lowercase())
        .collect()
}

// -- admission --------------------------------------------------------------------------------

/// Why a `POST /chat` was not accepted, in the currency of its HTTP response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatRefusal {
    /// 422: a rule refused the line.
    Rejected(RejectReason),
    /// 429: the per-name or global limit refused it.
    RateLimited { retry_after_ms: u64 },
}

impl ChatRefusal {
    /// The metric label for this refusal.
    pub fn reason(self) -> RejectReason {
        match self {
            Self::Rejected(reason) => reason,
            Self::RateLimited { .. } => RejectReason::RateLimited,
        }
    }
}

/// Per-name and global admission for `POST /chat`.
///
/// The global budget reuses [`RateLimiter`] over a one-second window; the per-name rule is a last
/// accepted timestamp per name, pruned as it goes so a raid cannot grow the map without bound.
#[derive(Debug)]
pub struct ChatLimiter {
    global: RateLimiter,
    per_name_ms: u64,
    last_by: HashMap<String, u64>,
}

impl Default for ChatLimiter {
    fn default() -> Self {
        Self::new(PER_SECOND, PER_NAME_MS)
    }
}

impl ChatLimiter {
    pub fn new(per_second: usize, per_name_ms: u64) -> Self {
        Self {
            global: RateLimiter::with_window(per_second, 1_000),
            per_name_ms,
            last_by: HashMap::new(),
        }
    }

    /// Admit one line from `by` at `now_ms`, or say how long to wait.
    ///
    /// The per-name rule is checked first: one viewer typing fast must not spend the global
    /// budget everyone else shares.
    pub fn admit(&mut self, by: &str, now_ms: u64) -> Result<(), ChatRefusal> {
        self.prune(now_ms);
        if let Some(last) = self.last_by.get(by) {
            let since = now_ms.saturating_sub(*last);
            if since < self.per_name_ms {
                return Err(ChatRefusal::RateLimited {
                    retry_after_ms: (self.per_name_ms - since).max(1),
                });
            }
        }
        // `admit` with no active pulse is the plain sliding-window check.
        if let Err(refusal) = self.global.admit(now_ms, 0.0) {
            return Err(ChatRefusal::RateLimited {
                retry_after_ms: refusal.retry_after_ms().max(1),
            });
        }
        self.last_by.insert(by.to_string(), now_ms);
        Ok(())
    }

    fn prune(&mut self, now_ms: u64) {
        if self.last_by.len() < 1_024 {
            return;
        }
        let horizon = self.per_name_ms;
        self.last_by
            .retain(|_, last| now_ms.saturating_sub(*last) < horizon);
    }
}

// -- the ring ---------------------------------------------------------------------------------

/// The ring's sidecar file, inside `[paths] hot_dir` (`docs/control-api.md`, `[chat]`).
///
/// The ring is session state, not simulation state, so it deliberately does **not** travel in the
/// `FLYSIM01` checkpoint envelope and is not in the compatibility string: a build that refuses
/// every checkpoint in a directory still reads this file, and a checkpoint written by any build
/// is byte-for-byte what it always was. It sits beside the hot checkpoints because it has their
/// lifetime — the tmpfs a reboot clears — and because the deliberate reset already clears that
/// directory (`infra/05-deploy.sh`, `FLY_RESET_STATE=1`).
///
/// Sharing that directory with the hot checkpoints means sharing its mtime, which the watchdog
/// reads as flysim's liveness (`infra/bin/fly-watchdog`, check 1: hot-state mtime younger than
/// 30 s). That is safe here only because this file is written from the sim thread, on the same
/// command path as the line itself: a wedged loop accepts no chat, so it can never refresh the
/// directory behind the watchdog's back. Nothing else may ever write here from another thread.
pub const SIDECAR_FILE: &str = "chat-ring.json";

/// Lines older than this are dropped when the sidecar is read: a panel coming back after a long
/// outage should be empty rather than show a day-old conversation as if it were live.
pub const SIDECAR_MAX_AGE_MS: u64 = 24 * 60 * 60 * 1_000;

/// The sidecar's own format version. Nothing else versions with it, which is the point.
const SIDECAR_VERSION: u32 = 1;

/// `<hot_dir>/chat-ring.json`.
pub fn sidecar_path(hot_dir: &Path) -> PathBuf {
    hot_dir.join(SIDECAR_FILE)
}

/// What the sidecar holds: a version and the ring, oldest first.
#[derive(Debug, Serialize, Deserialize)]
struct Sidecar {
    version: u32,
    lines: Vec<ChatLine>,
}

/// The last `capacity` accepted lines, oldest first, as every snapshot header carries them.
#[derive(Debug, Clone)]
pub struct ChatRing {
    capacity: usize,
    lines: VecDeque<ChatLine>,
}

impl ChatRing {
    pub fn new(capacity: usize) -> Self {
        Self { capacity: capacity.clamp(1, RING_MAX), lines: VecDeque::new() }
    }

    pub fn push(&mut self, line: ChatLine) {
        while self.lines.len() >= self.capacity {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    /// Oldest first, for the header.
    pub fn lines(&self) -> Vec<ChatLine> {
        self.lines.iter().cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Write the ring to `<hot_dir>/chat-ring.json`: tmp file, fsync, rename over, directory
    /// fsync — the same atomic sequence a checkpoint commit uses, so a reader never sees a
    /// half-written ring and a crash mid-write leaves the previous one.
    ///
    /// Called on every accepted line, which the admission limits cap at five a second, onto
    /// tmpfs.
    pub fn save_sidecar(&self, hot_dir: &Path) -> anyhow::Result<()> {
        let sidecar = Sidecar { version: SIDECAR_VERSION, lines: self.lines() };
        let bytes = serde_json::to_vec(&sidecar)?;
        crate::store::write_atomic(&sidecar_path(hot_dir), &bytes)
    }

    /// Read `<hot_dir>/chat-ring.json` into the ring, and answer how many lines it restored.
    ///
    /// Absent is silence and zero lines — the first run on a fresh box. Unreadable, unparseable
    /// or a version this build does not know is zero lines and a logged warning: an empty panel
    /// is exactly what a restart gives today, so nothing on this path may ever be fatal. Lines
    /// older than [`SIDECAR_MAX_AGE_MS`] are dropped, and only the newest `capacity` survive,
    /// whatever the file holds.
    pub fn load_sidecar(&mut self, hot_dir: &Path, now_ms: u64) -> usize {
        let path = sidecar_path(hot_dir);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!(
                        %error,
                        path = %path.display(),
                        "could not read the chat ring sidecar; the panel starts empty"
                    );
                }
                return 0;
            }
        };
        let sidecar: Sidecar = match serde_json::from_str(&text) {
            Ok(sidecar) => sidecar,
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "the chat ring sidecar is not readable; ignoring it"
                );
                return 0;
            }
        };
        if sidecar.version != SIDECAR_VERSION {
            tracing::warn!(
                version = sidecar.version,
                path = %path.display(),
                "the chat ring sidecar is a version this build does not read; ignoring it"
            );
            return 0;
        }

        let before = sidecar.lines.len();
        self.lines.clear();
        for line in sidecar.lines {
            if now_ms.saturating_sub(line.wall_ms) >= SIDECAR_MAX_AGE_MS {
                continue;
            }
            self.push(line);
        }
        let restored = self.lines.len();
        if restored < before {
            tracing::info!(
                dropped = before - restored,
                "dropped chat lines older than a day from the sidecar"
            );
        }
        restored
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_line_survives_and_whitespace_collapses() {
        assert_eq!(sanitize_chat_text("go left!").unwrap(), "go left!");
        assert_eq!(sanitize_chat_text("  a   b  ").unwrap(), "a b");
        assert_eq!(sanitize_chat_text("a\tb\nc").unwrap(), "a b c");
    }

    #[test]
    fn the_rejection_reasons_are_the_documented_ones_in_the_documented_order() {
        assert_eq!(sanitize_chat_text("a\u{0}b"), Err(RejectReason::Control));
        assert_eq!(sanitize_chat_text("a\u{200b}b"), Err(RejectReason::Charset));
        assert_eq!(sanitize_chat_text("   "), Err(RejectReason::Empty));
        assert_eq!(sanitize_chat_text(&"a".repeat(201)), Err(RejectReason::TooLong));
        assert_eq!(sanitize_chat_text("bit.ly"), Err(RejectReason::Url));
        // Control before charset, charset before length, length before URL.
        assert_eq!(sanitize_chat_text("\u{0}\u{1fab0}"), Err(RejectReason::Control));
        assert_eq!(
            sanitize_chat_text(&"\u{1fab0}".repeat(500)),
            Err(RejectReason::Charset)
        );
        assert_eq!(
            sanitize_chat_text(&"bit.ly ".repeat(100)),
            Err(RejectReason::TooLong)
        );
    }

    #[test]
    fn sanitizing_is_idempotent() {
        for line in ["go left!", "a   b", "na\u{ef}ve", "route 1 took 3.14 minutes"] {
            let once = sanitize_chat_text(line).unwrap();
            assert_eq!(sanitize_chat_text(&once).unwrap(), once, "{line}");
        }
    }

    #[test]
    fn display_names_are_letters_digits_and_underscore_only() {
        for name in ["alex", "fly_fan_42", "\u{96e8}\u{5bae}", "\u{3a9}_2", &"a".repeat(25)] {
            assert!(is_valid_display_name(name), "{name}");
        }
        for name in ["", "a viewer", "alex!", &"a".repeat(26), "he\u{200b}re"] {
            assert!(!is_valid_display_name(name), "{name:?}");
        }
    }

    #[test]
    fn a_deny_list_matches_the_name_or_the_text_case_insensitively() {
        let mut list = DenyList::empty(Instant::now());
        assert!(!list.blocks("alex", "anything at all"));

        list.patterns = parse_deny_list("# a comment\n\n  SlUr \nspoiler\n");
        assert_eq!(list.patterns(), ["slur", "spoiler"]);
        assert!(list.blocks("alex", "that is a SLUR"));
        assert!(list.blocks("alex", "Spoiler: the badge"));
        assert!(list.blocks("slur_fan_99", "hello"));
        assert!(!list.blocks("alex", "perfectly ordinary"));
    }

    #[test]
    fn a_missing_deny_list_file_is_an_empty_list_rather_than_a_failure() {
        let list = DenyList::load(Some(Path::new("/nonexistent/chat-deny.txt")), Instant::now());
        assert!(list.patterns().is_empty());
        assert!(!list.blocks("alex", "hello"));
    }

    #[test]
    fn the_deny_list_reloads_on_force_and_on_the_interval_but_not_in_between() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("chat-deny.txt");
        std::fs::write(&path, "# nothing yet\n").unwrap();
        let start = Instant::now();
        let mut list = DenyList::load(Some(&path), start);
        assert!(list.patterns().is_empty());

        std::fs::write(&path, "slur\n").unwrap();
        assert!(!list.maybe_reload(start + Duration::from_secs(30), false), "too early");
        assert!(list.patterns().is_empty());

        assert!(list.maybe_reload(start + Duration::from_secs(1), true), "SIGHUP");
        assert_eq!(list.patterns(), ["slur"]);

        std::fs::write(&path, "slur\nspoiler\n").unwrap();
        assert!(list.maybe_reload(start + Duration::from_secs(120), false), "interval elapsed");
        assert_eq!(list.patterns(), ["slur", "spoiler"]);
    }

    #[test]
    fn the_per_name_limit_is_one_line_every_two_seconds() {
        let mut limiter = ChatLimiter::default();
        limiter.admit("alex", 1_000).unwrap();
        assert_eq!(
            limiter.admit("alex", 1_500),
            Err(ChatRefusal::RateLimited { retry_after_ms: 1_500 })
        );
        limiter.admit("other", 1_500).unwrap();
        limiter.admit("alex", 3_000).unwrap();
    }

    #[test]
    fn the_global_limit_is_five_a_second_and_a_refused_line_costs_no_budget() {
        let mut limiter = ChatLimiter::default();
        for index in 0..5 {
            limiter.admit(&format!("viewer_{index}"), 1_000 + index).unwrap();
        }
        let refusal = limiter.admit("viewer_5", 1_005).unwrap_err();
        assert!(matches!(refusal, ChatRefusal::RateLimited { .. }));
        // The refused name is not recorded, so it is not also per-name limited a second later.
        limiter.admit("viewer_5", 2_001).unwrap();
    }

    #[test]
    fn a_flood_never_admits_more_than_the_global_budget_in_one_second() {
        let mut limiter = ChatLimiter::default();
        let mut admitted = 0;
        for step in 0..1_000u64 {
            if limiter.admit(&format!("viewer_{step}"), step).is_ok() {
                admitted += 1;
            }
        }
        // 1,000 distinct names over 999 ms: the global window admits five.
        assert_eq!(admitted, 5);
    }

    #[test]
    fn the_ring_keeps_the_newest_lines_and_never_exceeds_its_capacity() {
        let mut ring = ChatRing::new(3);
        for id in 1..=5 {
            ring.push(ChatLine {
                id,
                wall_ms: 1_757_000_000_000 + id,
                by: format!("viewer_{id}"),
                text: format!("line {id}"),
                bot: None,
            });
        }
        assert_eq!(ring.len(), 3);
        let lines = ring.lines();
        assert_eq!(lines.first().unwrap().id, 3, "oldest first");
        assert_eq!(lines.last().unwrap().id, 5);

        // The capacity is clamped to the contract's ceiling whatever the config says.
        assert_eq!(ChatRing::new(0).capacity(), 1);
        assert_eq!(ChatRing::new(999).capacity(), RING_MAX);
    }

    /// A line `wall_ms` milliseconds into the wall clock, for the sidecar tests.
    fn line(id: u64, wall_ms: u64) -> ChatLine {
        ChatLine {
            id,
            wall_ms,
            by: format!("viewer_{id}"),
            text: format!("line {id}"),
            bot: None,
        }
    }

    #[test]
    fn the_ring_round_trips_through_its_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = 1_757_000_000_000;

        // Nothing written yet: a fresh box is an empty ring and no complaint.
        let mut cold = ChatRing::new(12);
        assert_eq!(cold.load_sidecar(dir.path(), now_ms), 0);
        assert!(cold.is_empty());

        let mut ring = ChatRing::new(12);
        ring.push(line(1, now_ms - 3_000));
        ring.push(line(2, now_ms - 2_000));
        ring.push(ChatLine { bot: Some(true), ..line(3, now_ms - 1_000) });
        ring.save_sidecar(dir.path()).unwrap();

        // Beside the hot checkpoints, under the documented name, and nothing else is written.
        assert!(sidecar_path(dir.path()).is_file());
        let written: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(written, [SIDECAR_FILE]);

        let mut restored = ChatRing::new(12);
        assert_eq!(restored.load_sidecar(dir.path(), now_ms), 3);
        assert_eq!(restored.lines(), ring.lines(), "oldest first, bot flag and all");

        // A smaller ring than the file keeps the newest lines, not the first three it reads.
        let mut small = ChatRing::new(2);
        assert_eq!(small.load_sidecar(dir.path(), now_ms), 2);
        assert_eq!(
            small.lines().iter().map(|line| line.id).collect::<Vec<_>>(),
            [2, 3]
        );
    }

    #[test]
    fn a_sidecar_that_will_not_parse_is_ignored_rather_than_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = 1_757_000_000_000;

        for content in [
            "",
            "{ not json at all",
            r#"{"version":1,"lines":[{"id":"not a number"}]}"#,
            r#"{"version":1}"#,
            // A format from some future build: readable JSON, unreadable meaning.
            r#"{"version":99,"lines":[{"id":1,"wallMs":1757000000000,"by":"a","text":"b"}]}"#,
        ] {
            std::fs::write(sidecar_path(dir.path()), content).unwrap();
            let mut ring = ChatRing::new(12);
            assert_eq!(ring.load_sidecar(dir.path(), now_ms), 0, "{content}");
            assert!(ring.is_empty(), "{content}");
        }

        // And the next accepted line simply writes a good one over it.
        let mut ring = ChatRing::new(12);
        ring.push(line(7, now_ms));
        ring.save_sidecar(dir.path()).unwrap();
        let mut back = ChatRing::new(12);
        assert_eq!(back.load_sidecar(dir.path(), now_ms), 1);
    }

    #[test]
    fn sidecar_lines_older_than_a_day_are_dropped_on_load() {
        let dir = tempfile::tempdir().unwrap();
        let now_ms = 1_757_000_000_000;

        let mut ring = ChatRing::new(12);
        ring.push(line(1, now_ms - SIDECAR_MAX_AGE_MS - 1));
        ring.push(line(2, now_ms - SIDECAR_MAX_AGE_MS));
        ring.push(line(3, now_ms - SIDECAR_MAX_AGE_MS + 1));
        ring.push(line(4, now_ms - 1_000));
        ring.save_sidecar(dir.path()).unwrap();

        let mut restored = ChatRing::new(12);
        assert_eq!(restored.load_sidecar(dir.path(), now_ms), 2, "24 h exactly is too old");
        assert_eq!(
            restored.lines().iter().map(|line| line.id).collect::<Vec<_>>(),
            [3, 4]
        );

        // A day later still, the whole file is stale and the panel starts empty.
        let mut later = ChatRing::new(12);
        assert_eq!(later.load_sidecar(dir.path(), now_ms + SIDECAR_MAX_AGE_MS), 0);
        assert!(later.is_empty());
    }

    #[test]
    fn every_reason_has_a_stable_spelling_and_a_unique_index() {
        let mut seen = std::collections::HashSet::new();
        for reason in RejectReason::ALL {
            assert!(seen.insert(reason.as_str()), "{} is duplicated", reason.as_str());
            assert_eq!(RejectReason::ALL[reason.index()], reason);
        }
        assert_eq!(seen.len(), RejectReason::ALL.len());
    }
}
