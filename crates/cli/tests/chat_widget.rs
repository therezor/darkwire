//! What the prompt shows, and where each row goes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a fixture that does not hold is a failing test either way"
)]

use darkwire::chat_widget::{ChatWidget, SummaryDefaults, Typed};
use darkwire::commands::command_rows;
use darkwire::history_cell::FoldLabels;
use darkwire::i18n::Translations;
use darkwire::pickers::palette::{PaletteRow, command_items};
use darkwire::render::{LineKind, TranscriptEvent};
use darkwire_protocol::config::ReasoningDisplay;
use darkwire_tui::{HistoryCell, Key, KeyName, strip_ansi};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Line;

/// A widget on an eighty by twenty-four window.
fn widget() -> ChatWidget {
    widget_with(SummaryDefaults::default())
}

fn widget_with(defaults: SummaryDefaults) -> ChatWidget {
    let t = Translations::new(darkwire_i18n::DEFAULT_LOCALE);
    let mut widget = ChatWidget::new(
        darkwire_tui::theme_for(Some(false)),
        FoldLabels {
            thinking: "thinking".to_owned(),
            reasoning: "reasoning".to_owned(),
            too_small: "window too small".to_owned(),
        },
        "generating",
    );
    widget.set_defaults(defaults);
    widget.set_screen_size(80, 24);
    // The real table, so the list a slash opens is the one an operator sees
    // rather than a fixture that cannot go stale.
    let rows: Vec<PaletteRow> = command_rows().iter().map(PaletteRow::from).collect();
    widget.set_commands(command_items(&rows, &t));
    widget
}

/// The key a terminal would have sent these bytes for.
fn key(bytes: &str) -> Key {
    match bytes {
        "\r" | "\n" => Key::named(KeyName::Enter),
        "\t" => Key::named(KeyName::Tab),
        "\u{1b}" => Key::named(KeyName::Escape),
        "\u{1b}[A" => Key::named(KeyName::Up),
        "\u{1b}[B" => Key::named(KeyName::Down),
        other => {
            let mut chars = other.chars();
            let first = chars.next().expect("a fixture has to name a key");
            assert!(chars.next().is_none(), "unnamed sequence: {other:?}");
            if ('\u{1}'..='\u{1a}').contains(&first) {
                return Key::ctrl(char::from(b'a' + (first as u8) - 1));
            }
            Key::char(first)
        }
    }
}

fn typed(widget: &mut ChatWidget, text: &str) {
    for character in text.chars() {
        assert_eq!(
            widget.handle_key(&key(&character.to_string())),
            Typed::Redraw,
            "an ordinary character submitted something"
        );
    }
}

/// The text of the rows the widget has written for the terminal.
fn written(widget: &mut ChatWidget) -> Vec<String> {
    widget.drain_history().iter().map(text_of).collect()
}

fn text_of(line: &Line<'static>) -> String {
    strip_ansi(
        &line
            .spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>(),
    )
    .trim_end()
    .to_owned()
}

/// The whole transcript, as the overlay would show it.
fn transcript(widget: &ChatWidget) -> Vec<String> {
    widget
        .cells()
        .iter()
        .flat_map(|cell| cell.transcript_lines(80))
        .map(|line| text_of(&line))
        .collect()
}

/// The rows the live area draws.
fn drawn(widget: &mut ChatWidget) -> Vec<String> {
    let height = widget.desired_height(80);
    let area = Rect::new(0, 0, 80, height);
    let mut buffer = Buffer::empty(area);
    widget.render(area, &mut buffer);
    (0..height)
        .map(|row| {
            (0..80)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

fn delta(text: &str) -> TranscriptEvent {
    TranscriptEvent::AssistantDelta {
        text: text.to_owned(),
        depth: 0,
    }
}

// ------------------------------------------------------------ what is written

#[test]
fn a_message_is_written_as_soon_as_it_is_said() {
    let mut widget = widget();
    widget.echo("what is this");

    assert_eq!(written(&mut widget), vec!["", "› what is this"]);
}

#[test]
fn a_finished_line_of_an_answer_reaches_the_terminal_before_the_turn_ends() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&delta("a finished line\nstill going"));

    // Long enough answers flush early; a short one waits for the turn to end,
    // because the live area can still hold it.
    assert!(written(&mut widget).is_empty());
    assert!(
        drawn(&mut widget).join("\n").contains("a finished line"),
        "the line is not on the screen either"
    );
}

#[test]
fn an_answer_is_written_when_the_turn_ends() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&delta("the answer\n"));
    widget.end_turn();

    assert_eq!(written(&mut widget), vec!["the answer"]);
}

#[test]
fn an_answer_interrupted_mid_line_still_says_what_it_said() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&delta("half a sen"));
    widget.end_turn();

    assert_eq!(written(&mut widget), vec!["half a sen"]);
}

#[test]
fn an_answer_too_long_for_the_live_area_reaches_the_terminal_as_it_goes() {
    let mut widget = widget();
    widget.start_turn();
    for at in 0..30 {
        widget.handle_event(&delta(&format!("line {at}\n")));
    }
    // Drawing is what notices the cap, because that is when the height is
    // worked out.
    let _ = drawn(&mut widget);

    let out = written(&mut widget);
    assert!(
        !out.is_empty(),
        "nothing reached the terminal while the answer was still arriving"
    );
    assert_eq!(out[0], "line 0", "the oldest rows are the ones that go");
}

#[test]
fn the_live_area_never_takes_more_than_half_the_window() {
    let mut widget = widget();
    widget.start_turn();
    for at in 0..40 {
        widget.handle_event(&delta(&format!("line {at}\n")));
    }

    assert!(
        widget.desired_height(80) < 24,
        "the live area filled the window"
    );
}

#[test]
fn the_turn_clock_keeps_running_once_the_answer_has_started() {
    // It used to stop on the first word of the answer, because one field was
    // both the clock and the spinner's frame. Every run of reasoning after
    // that reported `reasoning 0ms`, which is what the second round of a turn
    // that calls tools always is.
    let mut widget = widget();
    widget.start_turn();
    for _ in 0..8 {
        widget.tick();
    }
    widget.handle_event(&delta("Let me look."));
    for _ in 0..40 {
        assert!(widget.tick(), "the clock stopped mid-turn");
    }

    widget.handle_event(&TranscriptEvent::ReasoningStart { elapsed_ms: None });
    widget.handle_event(&TranscriptEvent::ReasoningDelta {
        text: "now what\n".to_owned(),
        depth: 0,
    });
    for _ in 0..20 {
        widget.tick();
    }
    widget.handle_event(&TranscriptEvent::ReasoningEnd);

    let summary = written(&mut widget)
        .into_iter()
        .find(|row| row.contains("reasoning"))
        .expect("the fold summary");
    assert!(
        !summary.contains("0ms"),
        "the second run was timed on a stopped clock: {summary:?}"
    );
}

#[test]
fn the_bar_spins_again_while_a_turn_waits_on_the_model_after_a_tool() {
    // The wait between a tool result and the answer to it had nothing moving
    // on screen at all, so a turn that called three tools looked stuck.
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&delta("checking."));
    widget.handle_event(&TranscriptEvent::ToolBodyStart {
        summary: "  ✓ 1ms".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::ToolBodyEnd);
    let quiet = drawn(&mut widget).join("\n");
    assert!(!quiet.contains("generating"), "{quiet:?}");

    widget.tick();
    assert!(
        drawn(&mut widget).join("\n").contains("generating"),
        "nothing says the turn is still going"
    );
}

// --------------------------------------------------------------- the folds

#[test]
fn reasoning_is_written_as_one_summary_row() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ReasoningStart { elapsed_ms: None });
    widget.handle_event(&TranscriptEvent::ReasoningDelta {
        text: "first thought\nsecond thought\n".to_owned(),
        depth: 1,
    });
    widget.handle_event(&TranscriptEvent::ReasoningEnd);

    let out = written(&mut widget);
    assert_eq!(out.len(), 1, "the body was written too: {out:?}");
    assert!(out[0].contains("reasoning"), "no summary row: {out:?}");
}

#[test]
fn the_reasoning_nobody_saw_is_still_in_the_transcript() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ReasoningStart { elapsed_ms: None });
    widget.handle_event(&TranscriptEvent::ReasoningDelta {
        text: "a thought\n".to_owned(),
        depth: 1,
    });
    widget.handle_event(&TranscriptEvent::ReasoningEnd);

    assert!(
        transcript(&widget)
            .iter()
            .any(|row| row.contains("a thought")),
        "the body was lost: {:?}",
        transcript(&widget)
    );
}

#[test]
fn reasoning_arrives_open_when_the_install_asked_for_it() {
    let mut widget = widget_with(SummaryDefaults {
        reasoning: ReasoningDisplay::Expanded,
        ..SummaryDefaults::default()
    });
    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ReasoningStart { elapsed_ms: None });
    widget.handle_event(&TranscriptEvent::ReasoningDelta {
        text: "a thought\n".to_owned(),
        depth: 1,
    });
    widget.handle_event(&TranscriptEvent::ReasoningEnd);

    let out = written(&mut widget);
    assert!(
        out.iter().any(|row| row.contains("a thought")),
        "the body was folded away: {out:?}"
    );
}

#[test]
fn a_tools_output_is_written_as_one_summary_row() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ToolBodyStart {
        summary: "ran a command".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::Line {
        kind: LineKind::Notice,
        text: "the output".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::ToolBodyEnd);

    assert_eq!(written(&mut widget), vec!["ran a command"]);
    assert!(transcript(&widget).iter().any(|row| row == "the output"));
}

#[test]
fn ctrl_o_decides_how_the_next_tool_arrives_not_the_last_one() {
    let mut widget = widget();
    widget.start_turn();
    widget.handle_event(&TranscriptEvent::ToolBodyStart {
        summary: "first call".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::Line {
        kind: LineKind::Notice,
        text: "first output".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::ToolBodyEnd);
    let before = written(&mut widget);

    assert_eq!(widget.handle_key(&key("\u{f}")), Typed::ToggleTools);

    widget.handle_event(&TranscriptEvent::ToolBodyStart {
        summary: "second call".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::Line {
        kind: LineKind::Notice,
        text: "second output".to_owned(),
    });
    widget.handle_event(&TranscriptEvent::ToolBodyEnd);
    let after = written(&mut widget);

    assert_eq!(before, vec!["first call"], "the first call was not folded");
    assert_eq!(
        after,
        vec!["second call", "second output"],
        "the key did not reach the next call"
    );
}

#[test]
fn what_a_turn_cost_is_hidden_by_default_and_kept_for_the_transcript() {
    let mut widget = widget();
    widget.handle_event(&TranscriptEvent::TurnStats {
        line: "1.2k tokens".to_owned(),
        shown: false,
    });

    assert!(written(&mut widget).is_empty());
    assert_eq!(transcript(&widget), vec!["1.2k tokens"]);
}

#[test]
fn ctrl_y_decides_whether_the_next_cost_is_written() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{19}")), Typed::ToggleStats);

    widget.handle_event(&TranscriptEvent::TurnStats {
        line: "1.2k tokens".to_owned(),
        shown: true,
    });

    assert_eq!(written(&mut widget), vec!["1.2k tokens"]);
}

#[test]
fn ctrl_y_tells_the_renderer_what_it_did() {
    let mut widget = widget();
    assert!(widget.take_stats_toggle().is_none());

    widget.handle_key(&key("\u{19}"));
    assert_eq!(widget.take_stats_toggle(), Some(true));
    assert!(
        widget.take_stats_toggle().is_none(),
        "the switch was reported twice"
    );
}

#[test]
fn the_output_command_sets_the_same_switch_a_key_does() {
    let mut widget = widget();
    widget.handle_event(&TranscriptEvent::StatsShown(true));
    assert!(widget.stats_shown());

    widget.handle_event(&TranscriptEvent::StatsShown(false));
    assert!(!widget.stats_shown());
}

// ---------------------------------------------------------------- the keys

#[test]
fn return_submits_what_was_typed() {
    let mut widget = widget();
    typed(&mut widget, "hello");

    assert_eq!(widget.typing(), "hello");
    assert_eq!(
        widget.handle_key(&key("\r")),
        Typed::Line("hello".to_owned())
    );
}

#[test]
fn ctrl_c_interrupts() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{3}")), Typed::Interrupt);
}

#[test]
fn ctrl_d_on_an_empty_line_leaves() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{4}")), Typed::Leave);
}

#[test]
fn ctrl_g_opens_the_palette() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{7}")), Typed::Palette);
}

#[test]
fn ctrl_t_opens_the_transcript() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{14}")), Typed::Transcript);
}

#[test]
fn ctrl_l_throws_the_screen_away() {
    let mut widget = widget();
    assert_eq!(widget.handle_key(&key("\u{c}")), Typed::Reset);
}

#[test]
fn a_slash_opens_the_command_list() {
    let mut widget = widget();
    typed(&mut widget, "/mo");

    assert!(
        drawn(&mut widget).join("\n").contains("/model"),
        "no command list: {:?}",
        drawn(&mut widget)
    );
}

#[test]
fn escape_closes_the_command_list_and_leaves_the_line_alone() {
    let mut widget = widget();
    typed(&mut widget, "/mo");

    assert_eq!(widget.handle_key(&key("\u{1b}")), Typed::Redraw);
    assert_eq!(widget.typing(), "/mo");
    assert!(!drawn(&mut widget).join("\n").contains("/model"));
}

#[test]
fn tab_completes_the_command_rather_than_running_it() {
    let mut widget = widget();
    typed(&mut widget, "/mod");
    assert_eq!(widget.handle_key(&key("\t")), Typed::Redraw);

    assert_eq!(widget.typing(), "/model ");
}

#[test]
fn a_space_closes_the_list_because_what_follows_is_an_argument() {
    let mut widget = widget();
    typed(&mut widget, "/model ");

    assert!(!drawn(&mut widget).join("\n").contains("/models"));
}

// ------------------------------------------------------------- the layout

#[test]
fn the_composer_is_at_the_bottom_of_the_live_area() {
    let mut widget = widget();
    typed(&mut widget, "hello");

    let rows = drawn(&mut widget);
    let composer = rows
        .iter()
        .position(|row| row.contains("hello"))
        .expect("the composer");
    // Under the rule that opens the pane, with only the slot below it: blank
    // rows and the bar, and nothing of the conversation.
    assert!(
        rows[composer - 1].starts_with('─'),
        "the composer is not under its rule: {rows:?}"
    );
    assert!(
        !rows[composer + 1..].iter().any(|row| row.contains("hello")),
        "something of the conversation is below the composer: {rows:?}"
    );
}

#[test]
fn the_height_it_reports_is_the_rows_it_draws() {
    let mut widget = widget();
    typed(&mut widget, "hello");

    assert_eq!(
        usize::from(widget.desired_height(80)),
        drawn(&mut widget).len()
    );
}

#[test]
fn a_window_too_small_for_a_prompt_says_so() {
    let mut widget = widget();
    widget.set_screen_size(80, 3);

    assert_eq!(widget.desired_height(80), 1);
    assert!(
        drawn(&mut widget).join("").contains("too small"),
        "no explanation: {:?}",
        drawn(&mut widget)
    );
}

#[test]
fn the_caret_is_where_the_typing_is() {
    let mut widget = widget();
    typed(&mut widget, "hello");

    let height = widget.desired_height(80);
    let caret = widget
        .cursor_pos(Rect::new(0, 0, 80, height))
        .expect("a caret");
    assert!(caret.1 < height, "the caret is off the live area");
}

// ------------------------------------------------------------- the history

#[test]
fn a_replay_writes_how_a_turn_went_as_well_as_what_it_said() {
    // It used to write the questions and the answers and drop the rest, so a
    // session somebody came back to had no reasoning and no calls in it, and
    // Ctrl-T on one showed nothing either. The events here are the ones a live
    // turn emits, which is the whole point: one renderer, two clocks.
    let mut widget = widget();
    widget.replay(&[
        TranscriptEvent::Line {
            kind: LineKind::Echo,
            text: "a question".to_owned(),
        },
        TranscriptEvent::ReasoningStart { elapsed_ms: None },
        TranscriptEvent::ReasoningDelta {
            text: "weighing it up".to_owned(),
            depth: 0,
        },
        TranscriptEvent::EndLine,
        TranscriptEvent::ReasoningEnd,
        TranscriptEvent::Line {
            kind: LineKind::ToolCall,
            text: "⚙ read".to_owned(),
        },
        TranscriptEvent::ToolBodyStart {
            summary: "  ✓".to_owned(),
        },
        TranscriptEvent::ToolBodyEnd,
        TranscriptEvent::AssistantDelta {
            text: "an answer".to_owned(),
            depth: 0,
        },
    ]);

    let out = written(&mut widget);
    assert!(out.iter().any(|row| row == "› a question"), "{out:?}");
    assert!(out.iter().any(|row| row.contains("⚙ read")), "{out:?}");
    assert!(out.iter().any(|row| row == "an answer"), "{out:?}");
    // The reasoning folds to its summary, the way it folds while it is live.
    assert!(out.iter().any(|row| row.contains("reasoning")), "{out:?}");
}

#[test]
fn a_replayed_run_of_reasoning_claims_no_duration() {
    // The store keeps what was said, not how long the saying took. A summary
    // counting from a clock that started when the prompt opened would put a
    // figure on it, and the figure would be wrong.
    let mut widget = widget();
    widget.replay(&[
        TranscriptEvent::ReasoningStart { elapsed_ms: None },
        TranscriptEvent::ReasoningDelta {
            text: "weighing it up".to_owned(),
            depth: 0,
        },
        TranscriptEvent::EndLine,
        TranscriptEvent::ReasoningEnd,
    ]);

    let summary = written(&mut widget)
        .into_iter()
        .find(|row| row.contains("reasoning"))
        .expect("the fold summary");
    assert_eq!(summary.trim(), "┄ reasoning", "{summary:?}");
}

#[test]
fn a_replayed_run_shows_the_figure_the_row_stored() {
    // The three-way in `summary_for`: a stored figure beats both the clock and
    // the silence a row without one falls back to.
    let mut widget = widget();
    widget.replay(&[
        TranscriptEvent::ReasoningStart {
            elapsed_ms: Some(4200),
        },
        TranscriptEvent::ReasoningDelta {
            text: "weighing it up".to_owned(),
            depth: 0,
        },
        TranscriptEvent::EndLine,
        TranscriptEvent::ReasoningEnd,
    ]);

    let summary = written(&mut widget)
        .into_iter()
        .find(|row| row.contains("reasoning"))
        .expect("the fold summary");
    assert_eq!(summary.trim(), "┄ reasoning 4.2s", "{summary:?}");
}

#[test]
fn a_replay_commits_an_answer_that_ended_without_a_newline() {
    // `EndLine` settles an open line; it does not commit one. The last answer
    // in a conversation used to stay in the stream rather than reach the rows.
    let mut widget = widget();
    widget.replay(&[TranscriptEvent::AssistantDelta {
        text: "the last word".to_owned(),
        depth: 0,
    }]);

    assert!(
        written(&mut widget)
            .iter()
            .any(|row| row == "the last word"),
        "the tail never reached the scrollback"
    );
}

#[test]
fn moving_to_another_conversation_says_where_the_last_one_ended() {
    let mut widget = widget();
    widget.echo("in the first session");
    let _ = written(&mut widget);

    widget.reopen("a header", &[]);

    let out = written(&mut widget);
    assert!(
        out.iter()
            .any(|row| row.chars().all(|glyph| glyph == '─') && !row.is_empty()),
        "no rule between the two: {out:?}"
    );
    assert!(out.iter().any(|row| row == "a header"));
}

#[test]
fn the_transcript_starts_again_at_the_conversation_being_read() {
    let mut widget = widget();
    widget.echo("in the first session");
    widget.reopen("a header", &[]);

    assert!(
        !transcript(&widget)
            .iter()
            .any(|row| row.contains("in the first session")),
        "the transcript ran two sessions together"
    );
}

#[test]
fn a_queued_line_comes_back_in_the_order_it_was_put_in() {
    let mut widget = widget();
    widget.queue("first".to_owned());
    widget.queue("second".to_owned());

    assert_eq!(widget.take_queued().as_deref(), Some("first"));
    assert_eq!(widget.take_queued().as_deref(), Some("second"));
    assert_eq!(widget.take_queued(), None);
}

#[test]
fn a_plan_is_shown_above_the_composer_and_never_written() {
    let mut widget = widget();
    widget.handle_event(&TranscriptEvent::Tasks(vec![(
        darkwire_protocol::TaskStatus::Doing,
        "the task".to_owned(),
    )]));

    assert!(written(&mut widget).is_empty(), "the plan was written");
    assert!(drawn(&mut widget).join("\n").contains("the task"));
}

#[test]
fn a_notice_outside_a_tool_run_is_a_row_of_its_own() {
    let mut widget = widget();
    widget.handle_event(&TranscriptEvent::Line {
        kind: LineKind::Notice,
        text: "attached to a session".to_owned(),
    });

    assert_eq!(written(&mut widget), vec!["attached to a session"]);
}

#[test]
fn opening_the_command_list_costs_the_conversation_nothing() {
    // The live area is at the foot of the screen, so growing it scrolls what
    // is above into the terminal's scrollback — and shrinking it again cannot
    // bring those rows back. A list that made the area taller would push the
    // conversation up a little every time one was opened.
    let mut widget = widget();
    let idle = widget.desired_height(80);

    typed(&mut widget, "/");
    let open = widget.desired_height(80);

    widget.handle_key(&key("\u{1b}"));
    let closed = widget.desired_height(80);

    assert_eq!(open, idle, "opening the list made the live area taller");
    assert_eq!(closed, idle, "closing it left rows behind");
}

#[test]
fn the_command_list_takes_the_bar_rather_than_pushing_it_down() {
    let mut widget = widget();
    widget.set_view(darkwire::header::HeaderView {
        model: "a-model".to_owned(),
        ..darkwire::header::HeaderView::default()
    });
    assert!(drawn(&mut widget).join("\n").contains("a-model"));

    typed(&mut widget, "/");

    let shown = drawn(&mut widget).join("\n");
    assert!(shown.contains("/help"), "no command list: {shown}");
    assert!(
        !shown.contains("a-model"),
        "the bar is still drawn beside the list: {shown}"
    );
}

#[test]
fn the_command_list_is_the_same_size_whatever_is_typed_into_it() {
    let mut widget = widget();
    typed(&mut widget, "/");
    let wide_open = widget.desired_height(80);

    typed(&mut widget, "mo");
    let filtered = widget.desired_height(80);

    assert_eq!(
        filtered, wide_open,
        "the list changed size as it was filtered"
    );
}

#[test]
fn a_message_of_several_lines_is_written_as_several_rows() {
    let mut widget = widget();
    widget.echo("first line\nsecond line");

    assert_eq!(
        written(&mut widget),
        vec!["", "› first line", "  second line"]
    );
}

#[test]
fn the_tail_of_the_conversation_can_be_asked_for_again() {
    let mut widget = widget();
    widget.echo("one");
    widget.echo("two");
    widget.echo("three");
    let _ = written(&mut widget);

    let tail = widget.history_tail(80, 2);
    let texts: Vec<String> = tail.iter().map(text_of).collect();

    assert_eq!(texts.len(), 2);
    assert_eq!(texts[1], "› three", "the tail is not the end: {texts:?}");
}

#[test]
fn a_tail_longer_than_the_conversation_is_the_conversation() {
    let mut widget = widget();
    widget.echo("only this");
    let _ = written(&mut widget);

    let texts: Vec<String> = widget.history_tail(80, 50).iter().map(text_of).collect();
    assert_eq!(texts, vec!["", "› only this"]);
}

#[test]
fn a_tail_of_nothing_is_nothing() {
    let widget = widget();
    assert!(widget.history_tail(80, 10).is_empty());
}
