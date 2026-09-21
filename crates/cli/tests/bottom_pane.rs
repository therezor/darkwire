//! The live rows at the bottom of the screen, and what stacks over them.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::bottom_pane::{BottomPane, SelectView, ViewKey};
use darkwire::header::HeaderView;
use darkwire::pickers::MenuAnswer;
use darkwire::pickers::palette::CommandChoice;
use darkwire_protocol::TaskStatus;
use darkwire_tui::{
    Editor, Key, KeyName, PLAIN_THEME, Select, SelectItem, SelectLabels, SelectOptions, strip_ansi,
};

/// A pane on a window `rows` tall.
fn pane(rows: usize) -> BottomPane {
    let mut pane = BottomPane::new(
        Editor::new(&PLAIN_THEME),
        HeaderView::default(),
        PLAIN_THEME,
    );
    pane.set_window_rows(rows);
    pane
}

/// The pane's rows at a width, escapes stripped.
fn rows(pane: &mut BottomPane, width: usize) -> Vec<String> {
    pane.render_rows(width)
        .into_lines()
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn menu(answer: tokio::sync::oneshot::Sender<Option<MenuAnswer>>) -> SelectView {
    let select = Select::new(SelectOptions {
        items: vec![SelectItem::new(0, "first"), SelectItem::new(1, "second")],
        labels: SelectLabels {
            title: "pick one".to_owned(),
            empty: "nothing".to_owned(),
            footer: "enter to choose".to_owned(),
            filter_prefix: None,
        },
        theme: None,
        max_rows: Some(4),
        index: None,
        actions: Vec::new(),
    });
    SelectView::new(select, answer)
}

#[test]
fn the_height_it_reports_is_the_rows_it_draws() {
    let mut pane = pane(24);
    assert_eq!(
        usize::from(pane.desired_height(40)),
        rows(&mut pane, 40).len()
    );
}

#[test]
fn a_plan_is_drawn_above_the_composer() {
    let mut pane = pane(24);
    pane.set_tasks(&[
        (TaskStatus::Done, "one".to_owned()),
        (TaskStatus::Doing, "two".to_owned()),
    ]);

    let drawn = rows(&mut pane, 40).join("\n");
    assert!(drawn.contains("one"), "the plan is not drawn: {drawn}");
    assert!(drawn.contains("two"));
}

#[test]
fn a_plan_longer_than_its_window_says_how_much_is_hidden() {
    let mut pane = pane(24);
    let tasks: Vec<(TaskStatus, String)> = (0..8)
        .map(|at| (TaskStatus::Todo, format!("task {at}")))
        .collect();
    pane.set_tasks(&tasks);

    let drawn = rows(&mut pane, 40).join("\n");
    assert!(drawn.contains("+5 more"), "no count of the rest: {drawn}");
}

#[test]
fn a_queued_message_is_drawn_above_the_composer() {
    let mut pane = pane(24);
    pane.queue("waiting".to_owned());

    let drawn = rows(&mut pane, 40).join("\n");
    assert!(drawn.contains("waiting"), "the queue is not drawn: {drawn}");
}

#[test]
fn a_queue_longer_than_its_window_says_how_much_is_hidden() {
    let mut pane = pane(24);
    for at in 0..6 {
        pane.queue(format!("message {at}"));
    }

    let drawn = rows(&mut pane, 40).join("\n");
    assert!(drawn.contains("+3 more"), "no count of the rest: {drawn}");
}

#[test]
fn a_queued_line_comes_back_in_the_order_it_was_put_in() {
    let mut pane = pane(24);
    pane.queue("first".to_owned());
    pane.queue("second".to_owned());

    assert_eq!(pane.take_queued().as_deref(), Some("first"));
    assert_eq!(pane.take_queued().as_deref(), Some("second"));
    assert_eq!(pane.take_queued(), None);
}

#[test]
fn a_spinner_is_drawn_while_a_turn_runs() {
    let mut pane = pane(24);
    pane.set_spinner(Some((0, "working".to_owned())));

    let drawn = rows(&mut pane, 40).join("\n");
    assert!(drawn.contains("working"), "no spinner row: {drawn}");

    pane.set_spinner(None);
    assert!(!rows(&mut pane, 40).join("\n").contains("working"));
}

#[test]
fn a_view_takes_the_keys_the_composer_would_have_had() {
    let mut pane = pane(24);
    let (answer, _receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));

    assert!(pane.has_view());
    assert_eq!(pane.offer_key(&Key::char('a')), ViewKey::Consumed);
    assert_eq!(pane.typing(), "", "the key reached the composer as well");
}

#[test]
fn a_view_that_answered_is_taken_away() {
    let mut pane = pane(24);
    let (answer, receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));

    pane.offer_key(&Key::named(KeyName::Enter));

    assert!(!pane.has_view(), "the menu stayed after choosing");
    assert_eq!(
        receiver.blocking_recv(),
        Ok(Some(MenuAnswer {
            row: 0,
            action: None
        }))
    );
}

#[test]
fn a_cancelled_view_answers_nothing_and_still_goes() {
    let mut pane = pane(24);
    let (answer, receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));

    pane.offer_key(&Key::named(KeyName::Escape));

    assert!(!pane.has_view());
    assert_eq!(receiver.blocking_recv(), Ok(None));
}

#[test]
fn a_key_reaches_the_composer_when_nothing_is_stacked_over_it() {
    let mut pane = pane(24);
    assert_eq!(pane.offer_key(&Key::char('a')), ViewKey::PassThrough);
}

#[test]
fn a_view_is_drawn_under_the_composer_and_not_over_it() {
    let mut pane = pane(24);
    let before = rows(&mut pane, 40);
    let rule = before
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the composer's rule");

    let (answer, _receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));
    let after = rows(&mut pane, 40);

    let first = after
        .iter()
        .position(|row| row.contains("first"))
        .expect("the menu");
    assert!(
        first > rule,
        "the menu was drawn over the composer: {after:?}"
    );
}

#[test]
fn a_menu_never_pushes_the_composer_off_a_small_window() {
    let pane = pane(8);
    assert!(pane.max_menu_rows() < 8);
    assert!(pane.max_menu_rows() >= 1);
}

#[test]
fn the_command_list_still_counts_what_it_is_not_showing() {
    // The list under a slash is not a menu: no title, no footer, no filter row
    // of its own, because the composer above it *is* the filter. What it does
    // have is the count, which moved out of the toolkit when the menus stopped
    // spending a row on one.
    let mut pane = pane(24);
    pane.set_commands(
        (0..9)
            .map(|at| {
                SelectItem::new(
                    CommandChoice {
                        command: format!("/c{at}"),
                        submit: true,
                    },
                    &format!("/c{at}"),
                )
            })
            .collect(),
    );
    pane.editor_mut().set_text("/c");
    pane.sync_popup();
    let drawn = rows(&mut pane, 40);

    assert!(
        drawn.iter().any(|row| row.contains("(1/9)")),
        "the count is gone: {drawn:?}"
    );
    assert!(
        !drawn.iter().any(|row| row.contains("choose")),
        "the list grew a menu's footer: {drawn:?}"
    );
}

#[test]
fn a_short_window_still_draws_the_composer_under_an_open_menu() {
    // The slot is sized from a constant that reserves the rest of the pane. It
    // was once the menu's own chrome constant, which is a different quantity,
    // and shrinking that one clipped the composer off the bottom here.
    let mut pane = pane(8);
    let (answer, _receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));
    let drawn = rows(&mut pane, 40);

    assert!(drawn.len() < 8, "the pane took the whole window: {drawn:?}");
    assert!(
        drawn.iter().any(|row| row.starts_with('─')),
        "the composer's rule was pushed off: {drawn:?}"
    );
}

#[test]
fn an_open_menu_asks_its_question_on_the_row_the_composer_had() {
    let mut pane = pane(24);
    let (answer, _receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));
    pane.offer_key(&Key::char('s'));
    let drawn = rows(&mut pane, 40);

    let rule = drawn
        .iter()
        .position(|row| row.starts_with('─'))
        .expect("the composer's rule");
    assert_eq!(
        drawn[rule + 1],
        "pick one s",
        "the question and the filter are not on the prompt row: {drawn:?}"
    );
    // And the menu does not repeat either of them below.
    assert!(
        !drawn[rule + 2..].iter().any(|row| row.contains("pick one")),
        "{drawn:?}"
    );
}

#[test]
fn the_composer_comes_back_when_the_menu_closes() {
    let mut pane = pane(24);
    pane.editor_mut().set_text("half a message");
    let (answer, _receiver) = tokio::sync::oneshot::channel();
    pane.push_view(Box::new(menu(answer)));
    assert!(!rows(&mut pane, 40).iter().any(|row| row.contains("half a")));

    pane.offer_key(&Key::named(KeyName::Escape));
    assert!(rows(&mut pane, 40).iter().any(|row| row.contains("half a")));
}

#[test]
fn a_long_message_is_shown_from_the_caret_backwards() {
    let mut pane = pane(9);
    // Long enough that the composer alone would fill the window.
    pane.editor_mut().set_text(&"word ".repeat(60));

    let drawn = rows(&mut pane, 20);
    assert!(
        drawn.len() < 20,
        "the composer took the whole window: {drawn:?}"
    );
}

#[test]
fn the_bar_at_the_bottom_says_what_it_was_given() {
    let mut pane = pane(24);
    pane.set_view(HeaderView {
        model: "a-model".to_owned(),
        ..HeaderView::default()
    });

    let drawn = strip_ansi(&rows(&mut pane, 60).join("\n"));
    assert!(drawn.contains("a-model"), "no model in the bar: {drawn}");
}
