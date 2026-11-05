//! Which way a terminal is told something is waiting, and the bytes for it.

use darkwire_tui::Alert;
use darkwire_tui::testkit::VT100Backend;
use std::io::Write;

fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
    move |name| {
        pairs
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| (*value).to_owned())
    }
}

#[test]
fn notifies_in_a_terminal_known_to_show_one() {
    for program in ["iTerm.app", "WezTerm", "ghostty"] {
        let pairs: &'static [(&str, &str)] =
            Box::leak(vec![("TERM_PROGRAM", program)].into_boxed_slice());
        assert_eq!(Alert::detect(env(pairs)), Alert::Osc9, "{program}");
    }
}

#[test]
fn speaks_kittys_own_protocol_to_kitty() {
    assert_eq!(Alert::detect(env(&[("TERM", "xterm-kitty")])), Alert::Osc99);
    assert_eq!(
        Alert::detect(env(&[("KITTY_WINDOW_ID", "1")])),
        Alert::Osc99
    );
}

#[test]
fn rings_in_a_terminal_it_does_not_know() {
    assert_eq!(
        Alert::detect(env(&[("TERM_PROGRAM", "Apple_Terminal")])),
        Alert::Bell
    );
    assert_eq!(Alert::detect(env(&[])), Alert::Bell);
}

#[test]
fn rings_under_tmux_whatever_runs_outside_it() {
    // The OSC needs passthrough to reach the outer terminal; the bell does not.
    assert_eq!(
        Alert::detect(env(&[
            ("TMUX", "/tmp/tmux-1/default"),
            ("TERM_PROGRAM", "iTerm.app")
        ])),
        Alert::Bell
    );
    // An empty value is a variable set to nothing, not a tmux.
    assert_eq!(
        Alert::detect(env(&[("TMUX", ""), ("TERM_PROGRAM", "iTerm.app")])),
        Alert::Osc9
    );
}

/// What a terminal makes of the bytes.
fn received(alert: Alert, text: &str) -> VT100Backend {
    let mut terminal = VT100Backend::new(40, 4);
    terminal.write_all(&alert.sequence(text)).unwrap();
    terminal
}

#[test]
fn sends_a_notification_that_draws_nothing_on_the_screen() {
    let terminal = received(Alert::Osc9, "exec is waiting for approval");
    assert_eq!(
        terminal.osc(),
        [vec![
            "9".to_owned(),
            "exec is waiting for approval".to_owned()
        ]]
    );
    assert_eq!(terminal.bells(), 0);
    assert!(terminal.rows().iter().all(String::is_empty));

    let terminal = received(Alert::Osc99, "exec is waiting for approval");
    assert_eq!(
        terminal.osc(),
        [vec![
            "99".to_owned(),
            String::new(),
            "exec is waiting for approval".to_owned()
        ]]
    );
}

#[test]
fn rings_once() {
    let terminal = received(Alert::Bell, "ignored");
    assert_eq!(terminal.bells(), 1);
    assert!(terminal.osc().is_empty());
}

#[test]
fn says_nothing_when_off() {
    assert!(Alert::Off.sequence("anything").is_empty());
}

#[test]
fn keeps_the_text_from_ending_the_sequence_early() {
    // A bell or an escape in the text would end the OSC there and print the
    // rest, or ring.
    let terminal = received(Alert::Osc9, "a\x07b\x1b\\c\x1b[31md");
    assert_eq!(terminal.bells(), 0);
    assert_eq!(terminal.osc(), [vec!["9".to_owned(), "abcd".to_owned()]]);
    assert!(terminal.rows().iter().all(String::is_empty));
}
