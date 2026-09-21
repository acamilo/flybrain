//! The Rust half of the shared sanitizer contract, plus the ring and the deny list end to end.
//!
//! Every hostile input lives in `packages/feed/tests/fixtures/chat-cases.json`, which
//! `packages/feed/tests/chat.test.ts` loads too. Neither suite keeps hostile cases of its own, so
//! a rule that changes in one language and not the other fails in both.
//!
//! `docs/control-api.md`: "the service additionally validates the name, caps text at 200 chars,
//! strips control characters and URLs, rejects non-printable or non-allowlisted characters, and
//! applies the deny list in `flysim.toml [chat]`".

mod common;

use flysim::chat::{
    ChatLimiter, ChatRefusal, ChatRing, DenyList, MAX_TEXT_LENGTH, RejectReason,
    is_valid_display_name, sanitize_chat_text,
};
use flysim::snapshot::ChatLine;
use serde_json::Value;

/// `packages/feed/tests/fixtures/chat-cases.json`.
fn fixture() -> Value {
    let path = common::repo_root().join("packages/feed/tests/fixtures/chat-cases.json");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("reading {}: {error}", path.display()));
    serde_json::from_str(&text).expect("the chat case fixture is valid JSON")
}

/// A fixture string: either a literal, or `{ "repeat": { "unit", "count" } }`. The TypeScript
/// loader expands it exactly the same way.
fn resolve(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    let repeat = value
        .get("repeat")
        .expect("a fixture string is a literal or a { repeat } object");
    let unit = repeat["unit"].as_str().expect("repeat.unit");
    let count = repeat["count"].as_u64().expect("repeat.count") as usize;
    unit.repeat(count)
}

fn reason_from_str(name: &str) -> RejectReason {
    RejectReason::ALL
        .into_iter()
        .find(|reason| reason.as_str() == name)
        .unwrap_or_else(|| panic!("the fixture names an unknown reason {name:?}"))
}

#[test]
fn the_shared_hostile_cases_all_behave_as_the_fixture_says() {
    let fixture = fixture();
    assert_eq!(
        fixture["maxTextLength"].as_u64().expect("maxTextLength"),
        MAX_TEXT_LENGTH as u64,
        "the fixture and this implementation disagree about the length cap"
    );
    let cases = fixture["cases"].as_array().expect("cases");
    assert!(cases.len() >= 30, "only {} shared cases", cases.len());

    let mut accepted = 0;
    let mut refused = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap_or("<unnamed>");
        let input = resolve(&case["input"]);
        let outcome = sanitize_chat_text(&input);
        if case["expected"].is_null() {
            let expected = reason_from_str(case["reason"].as_str().expect("reason"));
            assert_eq!(outcome, Err(expected), "{name}");
            refused += 1;
        } else {
            let expected = resolve(&case["expected"]);
            assert_eq!(outcome.as_deref(), Ok(expected.as_str()), "{name}");
            accepted += 1;
        }
    }
    assert!(accepted >= 8 && refused >= 20, "{accepted} accepted, {refused} refused");
}

#[test]
fn every_accepted_case_survives_a_second_pass_unchanged() {
    // The bridge sanitizes, then the service sanitizes again; a non-idempotent rule would drop
    // legitimate lines on the second look.
    for case in fixture()["cases"].as_array().expect("cases") {
        if case["expected"].is_null() {
            continue;
        }
        let once = sanitize_chat_text(&resolve(&case["input"])).expect("accepted");
        assert_eq!(sanitize_chat_text(&once).as_deref(), Ok(once.as_str()));
    }
}

#[test]
fn the_fixture_covers_every_reason_the_sanitizer_itself_can_return() {
    // `name`, `deny_list`, `rate_limited` and `malformed` are the service's own refusals, not the
    // sanitizer's, so they are exercised by the tests below and by `tests/api.rs` instead.
    let fixture = fixture();
    let mut seen: Vec<&str> = fixture["cases"]
        .as_array()
        .expect("cases")
        .iter()
        .filter_map(|case| case["reason"].as_str())
        .collect();
    seen.sort_unstable();
    seen.dedup();
    for reason in [
        RejectReason::Control,
        RejectReason::Charset,
        RejectReason::Empty,
        RejectReason::TooLong,
        RejectReason::Url,
    ] {
        assert!(seen.contains(&reason.as_str()), "no shared case for {}", reason.as_str());
    }
}

#[test]
fn the_name_rule_is_the_one_the_bridge_applies() {
    for name in ["alex", "fly_fan_42", "\u{96e8}\u{5bae}", &"n".repeat(25)] {
        assert!(is_valid_display_name(name), "{name}");
    }
    for name in ["", "a viewer", "fly-fan-42", "fan!", &"n".repeat(26), "<b>alex</b>"] {
        assert!(!is_valid_display_name(name), "{name:?}");
    }
}

#[test]
fn a_deny_list_pattern_refuses_a_line_that_the_sanitizer_would_otherwise_accept() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chat-deny.txt");
    std::fs::write(&path, "# operator-maintained\nspoiler\nslur\n").unwrap();
    let list = DenyList::load(Some(&path), std::time::Instant::now());

    let line = sanitize_chat_text("SPOILER: it gets the badge").expect("sanitizer accepts it");
    assert!(list.blocks("alex", &line), "the deny list has the last word");
    let fine = sanitize_chat_text("it gets the badge").expect("sanitizer accepts it");
    assert!(!list.blocks("alex", &fine));
}

#[test]
fn the_ring_is_bounded_and_carries_bot_lines_as_such() {
    let mut ring = ChatRing::new(3);
    for id in 1..=4 {
        ring.push(ChatLine {
            id,
            wall_ms: 1_757_000_000_000 + id,
            by: format!("viewer_{id}"),
            text: format!("line {id}"),
            bot: Some(id == 4).filter(|bot| *bot),
        });
    }
    let lines = ring.lines();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].id, 2, "the oldest line was dropped");
    assert_eq!(lines[2].bot, Some(true));
    assert_eq!(lines[0].bot, None, "a viewer line omits the field entirely");

    // Serialized, a viewer line has no `bot` key at all — the schema is strict about it.
    let json = serde_json::to_value(&lines[0]).unwrap();
    assert!(json.get("bot").is_none(), "{json}");
    assert_eq!(serde_json::to_value(&lines[2]).unwrap()["bot"], serde_json::json!(true));
}

#[test]
fn the_admission_limits_are_the_contract_numbers() {
    let mut limiter = ChatLimiter::default();
    // One line per name per 2 s.
    limiter.admit("alex", 10_000).unwrap();
    assert_eq!(
        limiter.admit("alex", 11_999),
        Err(ChatRefusal::RateLimited { retry_after_ms: 1 })
    );
    limiter.admit("alex", 12_000).unwrap();

    // Five lines a second across everyone.
    let mut limiter = ChatLimiter::default();
    for index in 0..5u64 {
        limiter.admit(&format!("viewer_{index}"), 50_000 + index * 10).unwrap();
    }
    let refusal = limiter.admit("viewer_5", 50_050).unwrap_err();
    assert!(matches!(refusal, ChatRefusal::RateLimited { .. }), "{refusal:?}");
    limiter.admit("viewer_5", 51_001).unwrap();
}

#[test]
fn a_hostile_line_can_never_reach_the_ring_by_any_route() {
    // The end-to-end invariant the rail depends on: everything in a ring came out of the
    // sanitizer, so re-sanitizing every line is a no-op and no line holds a control character, a
    // URL, an emoji or more than the cap.
    let mut ring = ChatRing::new(12);
    let hostile = [
        "https://evil.example",
        "zero\u{200b}width",
        "\u{0}nul",
        "\u{1fab0}",
        &"a".repeat(5_000),
        "   ",
    ];
    let mut id = 0;
    for candidate in hostile {
        if let Ok(text) = sanitize_chat_text(candidate) {
            id += 1;
            ring.push(ChatLine {
                id,
                wall_ms: 0,
                by: "alex".to_string(),
                text,
                bot: None,
            });
        }
    }
    assert!(ring.is_empty(), "a hostile line reached the ring: {:?}", ring.lines());

    for line in ["go left", "route 1, 3.14 minutes"] {
        id += 1;
        let text = sanitize_chat_text(line).unwrap();
        ring.push(ChatLine { id, wall_ms: 0, by: "alex".to_string(), text, bot: None });
    }
    for line in ring.lines() {
        assert_eq!(sanitize_chat_text(&line.text).as_deref(), Ok(line.text.as_str()));
        assert!(line.text.chars().count() <= MAX_TEXT_LENGTH);
        assert!(is_valid_display_name(&line.by));
    }
}
