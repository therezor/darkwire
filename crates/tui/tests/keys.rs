//! Bytes in, keys out: both cursor encodings, the modifier bitmask, whole chunks.

use ghostai_tui::{KeyName, is_ctrl, parse_key, parse_keys};

const ESC: &str = "\x1b";
const CSI: &str = "\x1b[";
const DEL: &str = "\x7f";

/// The name alone, for the cases where the modifiers are not the point.
fn name_of(data: &str) -> Option<KeyName> {
    parse_key(data).map(|key| key.name)
}

#[test]
fn decodes_the_cursor_keys_in_both_encodings() {
    // A terminal in DECCKM sends SS3 for the cursor keys, and plenty do so by
    // default. A decoder that only knows CSI silently ignores every one of
    // them, which is "the arrow keys do nothing on my machine".
    assert_eq!(name_of(&format!("{CSI}A")), Some(KeyName::Up));
    assert_eq!(name_of(&format!("{ESC}OA")), Some(KeyName::Up));
    assert_eq!(name_of(&format!("{CSI}B")), Some(KeyName::Down));
    assert_eq!(name_of(&format!("{ESC}OB")), Some(KeyName::Down));
    assert_eq!(name_of(&format!("{CSI}C")), Some(KeyName::Right));
    assert_eq!(name_of(&format!("{ESC}OC")), Some(KeyName::Right));
    assert_eq!(name_of(&format!("{CSI}D")), Some(KeyName::Left));
    assert_eq!(name_of(&format!("{ESC}OD")), Some(KeyName::Left));
}

#[test]
fn decodes_home_and_end_in_all_three_forms() {
    assert_eq!(name_of(&format!("{CSI}H")), Some(KeyName::Home));
    assert_eq!(name_of(&format!("{ESC}OH")), Some(KeyName::Home));
    assert_eq!(name_of(&format!("{CSI}1~")), Some(KeyName::Home));
    assert_eq!(name_of(&format!("{CSI}7~")), Some(KeyName::Home));
    assert_eq!(name_of(&format!("{CSI}F")), Some(KeyName::End));
    assert_eq!(name_of(&format!("{ESC}OF")), Some(KeyName::End));
    assert_eq!(name_of(&format!("{CSI}4~")), Some(KeyName::End));
    assert_eq!(name_of(&format!("{CSI}8~")), Some(KeyName::End));
}

#[test]
fn decodes_the_tilde_family() {
    assert_eq!(name_of(&format!("{CSI}3~")), Some(KeyName::Delete));
    assert_eq!(name_of(&format!("{CSI}5~")), Some(KeyName::PageUp));
    assert_eq!(name_of(&format!("{CSI}6~")), Some(KeyName::PageDown));
}

#[test]
fn reads_the_modifier_parameter_as_one_plus_bitmask() {
    // `\x1b[1;5A` is Ctrl-Up: 5 is 1 + 4, and 4 is the control bit.
    let ctrl = parse_key(&format!("{CSI}1;5A")).unwrap();
    assert_eq!((ctrl.name, ctrl.ctrl), (KeyName::Up, true));
    let shift = parse_key(&format!("{CSI}1;2A")).unwrap();
    assert_eq!((shift.name, shift.shift), (KeyName::Up, true));
    let meta = parse_key(&format!("{CSI}1;3A")).unwrap();
    assert_eq!((meta.name, meta.meta), (KeyName::Up, true));
    // A parameter of 1 means "no modifiers", not "shift".
    let plain = parse_key(&format!("{CSI}1;1A")).unwrap();
    assert_eq!(
        (plain.name, plain.ctrl, plain.shift, plain.meta),
        (KeyName::Up, false, false, false)
    );
    // The modifier applies to the tilde family too.
    let ctrl_delete = parse_key(&format!("{CSI}3;5~")).unwrap();
    assert_eq!(
        (ctrl_delete.name, ctrl_delete.ctrl),
        (KeyName::Delete, true)
    );
    // An empty second parameter is no modifier.
    let junk = parse_key(&format!("{CSI}1;A")).unwrap();
    assert_eq!((junk.name, junk.ctrl), (KeyName::Up, false));
    // A sub-parameter after a colon belongs to the terminal, not the modifier.
    let sub = parse_key(&format!("{CSI}1;5:3A")).unwrap();
    assert_eq!((sub.name, sub.ctrl), (KeyName::Up, true));
}

#[test]
fn decodes_csi_z_as_shift_tab() {
    let key = parse_key(&format!("{CSI}Z")).unwrap();
    assert_eq!((key.name, key.shift), (KeyName::Tab, true));
}

#[test]
fn names_enter_tab_and_backspace_rather_than_control_letters() {
    // A caller asking "did the user press Return" should not have to know that
    // Return is Ctrl-M.
    assert_eq!(name_of("\r"), Some(KeyName::Enter));
    assert_eq!(name_of("\n"), Some(KeyName::Enter));
    assert_eq!(name_of("\t"), Some(KeyName::Tab));
    assert_eq!(name_of(DEL), Some(KeyName::Backspace));
    assert_eq!(name_of("\x08"), Some(KeyName::Backspace));
}

#[test]
fn decodes_a_control_byte_as_the_letter_it_was_typed_with() {
    let key = parse_key("\x03").unwrap();
    assert_eq!(key.name, KeyName::Char);
    assert_eq!(key.character, "c");
    assert!(key.ctrl);
    let key = parse_key("\x15").unwrap();
    assert_eq!(key.character, "u");
    assert!(key.ctrl);
}

#[test]
fn decodes_an_ordinary_character() {
    let key = parse_key("a").unwrap();
    assert_eq!((key.name, key.character.as_str()), (KeyName::Char, "a"));
    let key = parse_key("A").unwrap();
    assert_eq!((key.name, key.character.as_str()), (KeyName::Char, "A"));
    assert!(!key.ctrl);
}

#[test]
fn keeps_an_astral_character_whole() {
    let keys = parse_keys("🚀");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].character, "🚀");
    assert_eq!(keys[0].sequence, "🚀");
}

#[test]
fn decodes_every_key_in_one_chunk() {
    // Holding an arrow key, or pasting, arrives as a single chunk. A decoder
    // that returned the first key would look like a menu skipping rows.
    let keys = parse_keys(&format!("{CSI}B{CSI}B\r"));
    let names: Vec<KeyName> = keys.iter().map(|key| key.name).collect();
    assert_eq!(names, [KeyName::Down, KeyName::Down, KeyName::Enter]);
}

#[test]
fn decodes_an_escape_followed_by_a_character_as_alt() {
    let key = parse_key(&format!("{ESC}b")).unwrap();
    assert_eq!(
        (key.name, key.character.as_str(), key.meta),
        (KeyName::Char, "b", true)
    );
    assert_eq!(key.sequence, format!("{ESC}b"));
}

#[test]
fn decodes_a_lone_escape_as_escape() {
    assert_eq!(name_of(ESC), Some(KeyName::Escape));
    assert_eq!(parse_keys(ESC).len(), 1);
}

#[test]
fn names_an_unrecognised_sequence_and_keeps_the_bytes() {
    let key = parse_key(&format!("{CSI}200~")).unwrap();
    assert_eq!(key.name, KeyName::Unknown);
    assert_eq!(key.sequence, format!("{CSI}200~"));

    let key = parse_key(&format!("{CSI}x")).unwrap();
    assert_eq!(key.name, KeyName::Unknown);

    let key = parse_key(&format!("{ESC}Ox")).unwrap();
    assert_eq!(key.name, KeyName::Unknown);
    assert_eq!(key.sequence, format!("{ESC}Ox"));

    // A control byte outside Ctrl-A..Ctrl-Z that has no name of its own.
    let key = parse_key("\x1c").unwrap();
    assert_eq!(key.name, KeyName::Unknown);
}

#[test]
fn recovers_the_rest_of_the_chunk_when_a_csi_is_never_terminated() {
    // A truncated read is noisy either way; what matters is that the remaining
    // bytes are still decoded rather than swallowed with the broken sequence.
    let keys = parse_keys(&format!("{CSI}12"));
    assert_eq!(keys[0].name, KeyName::Escape);
    let rest: String = keys.iter().map(|key| key.character.as_str()).collect();
    assert!(rest.contains("12"));

    // The same for a truncated SS3.
    let keys = parse_keys(&format!("{ESC}O"));
    assert_eq!(keys[0].name, KeyName::Escape);
    assert_eq!(keys[1].character, "O");
}

#[test]
fn returns_nothing_for_an_empty_chunk() {
    assert!(parse_keys("").is_empty());
    assert!(parse_key("").is_none());
}

#[test]
fn is_ctrl_matches_a_control_letter_case_insensitively() {
    let key = parse_key("\x07").unwrap();
    assert!(is_ctrl(&key, 'g'));
    assert!(is_ctrl(&key, 'G'));
    assert!(!is_ctrl(&key, 'h'));
}

#[test]
fn is_ctrl_answers_false_for_tab_enter_and_backspace() {
    // They are Ctrl-I, Ctrl-M and Ctrl-H on the wire. A menu that bound Ctrl-H
    // to something would otherwise be a menu that eats Backspace.
    for (data, letter) in [("\t", 'i'), ("\r", 'm'), ("\x08", 'h')] {
        let key = parse_key(data).unwrap();
        assert!(!is_ctrl(&key, letter), "{data:?} read as Ctrl-{letter}");
    }
}

#[test]
fn is_ctrl_answers_false_for_an_ordinary_character() {
    let key = parse_key("c").unwrap();
    assert!(!is_ctrl(&key, 'c'));
}
