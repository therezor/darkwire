//! The approval cards a chat can still edit, and what a card says.

use std::sync::Arc;
use std::time::Duration;

use darkwire_channels::projection::ApprovalDraftDetail;
use darkwire_channels::telegram::approvals::{
    ApprovalCard, ApprovalCards, MAX_APPROVAL_CARDS, approval_text, offered_rule, rule_label,
};
use darkwire_core::clock::Clock;
use darkwire_core::testkit::ManualClock;
use darkwire_protocol::{CommandPolicy, ExecRule, ToolPermission, ToolRisk};

const NOW: i64 = 1_700_000_000_000;

fn cards() -> (ApprovalCards, Arc<ManualClock>) {
    let clock = Arc::new(ManualClock::at(NOW));
    (
        ApprovalCards::new(Arc::clone(&clock) as Arc<dyn Clock>),
        clock,
    )
}

fn detail(call_id: &str, risk: ToolRisk, expires_in_ms: i64) -> ApprovalDraftDetail {
    ApprovalDraftDetail {
        call_id: call_id.to_owned(),
        name: "write".to_owned(),
        risk,
        expires_at_ms: u64::try_from(NOW + expires_in_ms).unwrap_or(0),
        command: None,
    }
}

fn card(call_id: &str, expires_in_ms: i64) -> ApprovalCard {
    ApprovalCard {
        chat_id: 4471,
        message_id: 1,
        session_key: "telegram:4471".to_owned(),
        detail: detail(call_id, ToolRisk::Write, expires_in_ms),
        text: "needs approval".to_owned(),
        answered: None,
    }
}

fn command(argv: &[&str], shell: bool) -> CommandPolicy {
    CommandPolicy {
        argv: argv.iter().map(|&token| token.to_owned()).collect(),
        shell,
        rule: None,
    }
}

#[test]
fn remembers_a_card_until_it_is_taken() {
    let (cards, _clock) = cards();
    assert!(cards.is_empty());

    cards.put(card("a", 60_000));

    assert_eq!(cards.len(), 1);
    assert_eq!(cards.get("a").map(|open| open.message_id), Some(1));
    assert!(cards.answer("a", Some("Approved once.".to_owned())));
    assert_eq!(
        cards.take("a").and_then(|open| open.answered),
        Some("Approved once.".to_owned())
    );
    assert!(cards.is_empty());
    assert!(!cards.answer("a", None), "a taken card is gone");
}

#[test]
fn forgets_a_card_whose_deadline_passed_when_the_next_one_arrives() {
    let (cards, clock) = cards();
    cards.put(card("old", 1_000));

    clock.advance(Duration::from_secs(2));
    cards.put(card("new", 60_000));

    assert!(cards.get("old").is_none());
    assert!(cards.get("new").is_some());
}

#[test]
fn keeps_no_more_than_its_bound_and_drops_the_oldest() {
    let (cards, _clock) = cards();
    for index in 0..=MAX_APPROVAL_CARDS {
        cards.put(card(&format!("c{index}"), 60_000));
    }

    assert_eq!(cards.len(), MAX_APPROVAL_CARDS);
    assert!(cards.get("c0").is_none());
    assert!(cards.get(&format!("c{MAX_APPROVAL_CARDS}")).is_some());
}

#[test]
fn prints_without_showing_what_it_holds() {
    let (cards, _clock) = cards();
    cards.put(card("a", 60_000));

    let printed = format!("{cards:?}");

    assert!(printed.contains("open: 1"), "{printed}");
    assert!(!printed.contains("telegram:4471"), "{printed}");
}

#[test]
fn names_every_risk_band_on_the_card() {
    for (risk, word) in [
        (ToolRisk::Safe, "safe"),
        (ToolRisk::Write, "write"),
        (ToolRisk::Exec, "exec"),
        (ToolRisk::Network, "network"),
    ] {
        let text = approval_text("needs approval", &detail("a", risk, 0));
        assert!(text.ends_with(&format!("risk: {word}")), "{text}");
    }
}

#[test]
fn offers_a_rule_for_a_program_but_never_for_a_shell() {
    assert_eq!(
        offered_rule(Some(&command(&["git", "log"], false))),
        Some(ExecRule {
            action: ToolPermission::Allow,
            argv: vec!["git".to_owned(), "log".to_owned(), "*".to_owned()],
        })
    );
    assert_eq!(
        offered_rule(Some(&command(&["sh", "-c", "ls"], true))),
        None
    );
    assert_eq!(offered_rule(None), None);
}

#[test]
fn cuts_a_long_rule_on_its_button() {
    let rule = ExecRule {
        action: ToolPermission::Allow,
        argv: vec!["x".repeat(60), "*".to_owned()],
    };

    let label = rule_label(&rule);

    assert!(label.starts_with("✅ Always: "), "{label}");
    assert!(label.ends_with('…'), "{label}");
}
