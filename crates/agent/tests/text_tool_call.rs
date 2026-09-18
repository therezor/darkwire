//! Recovering a call the model wrote into its answer instead of making.
//!
//! Detection is deliberately narrow: a name that matches no registered tool is
//! prose about tools, not an attempt to use one, and the correction is
//! worthless when the model was only explaining itself.

use darkwire_agent::text_tool_call::{text_tool_call_correction, text_tool_call_name};

fn known() -> Vec<String> {
    vec![
        "fetch".to_owned(),
        "read".to_owned(),
        "web.search".to_owned(),
    ]
}

#[test]
fn it_finds_the_transcript_that_motivated_it() {
    // Observed from a local model whose first call in the same turn was a
    // correctly structured one.
    let written = "The search tool is currently rate-limited. I will try using the `fetch` tool…

<tool_output>
<tool_call>
{\"name\": \"fetch\", \"arguments\": [\"https://www.bbc.com/news\"]}
</tool_call>
</tool_output>";

    assert_eq!(
        text_tool_call_name(written, &known()),
        Some("fetch".to_owned())
    );
}

#[test]
fn it_reads_every_wrapper_models_reach_for() {
    for tag in ["tool_call", "function_call", "tool_use"] {
        let written = format!("<{tag}>{{\"name\": \"fetch\"}}</{tag}>");
        assert_eq!(
            text_tool_call_name(&written, &known()),
            Some("fetch".to_owned()),
            "{tag}"
        );
        // The opening tag is matched case-insensitively, as models write it.
        let shouted = format!("<{}>{{\"name\": \"fetch\"}}</{}>", tag.to_uppercase(), tag);
        assert_eq!(
            text_tool_call_name(&shouted, &known()),
            Some("fetch".to_owned()),
            "{tag} shouted"
        );
    }
}

#[test]
fn it_reads_a_fenced_block_and_a_bare_object() {
    let fenced =
        "Here goes:\n\n```json\n{\"name\": \"read\", \"arguments\": {\"path\": \"a\"}}\n```";
    assert_eq!(
        text_tool_call_name(fenced, &known()),
        Some("read".to_owned())
    );

    let bare = "I'll do {\"name\": \"read\", \"arguments\": {}} now.";
    assert_eq!(text_tool_call_name(bare, &known()), Some("read".to_owned()));
}

#[test]
fn a_dotted_name_is_still_a_name() {
    let written = "<tool_call>{\"name\": \"web.search\"}</tool_call>";
    assert_eq!(
        text_tool_call_name(written, &known()),
        Some("web.search".to_owned())
    );
}

#[test]
fn prose_about_tools_is_left_alone() {
    // The correction is worthless when the model was only explaining itself.
    let cases = [
        "You would call fetch with a URL.",
        "The `read` tool takes a path.",
        "",
        "Nothing structured at all.",
        // A name that matches no registered tool.
        "<tool_call>{\"name\": \"delete_everything\"}</tool_call>",
        // The word `name` without a call around it.
        "Its \"name\" is unclear.",
    ];
    for case in cases {
        assert_eq!(text_tool_call_name(case, &known()), None, "{case}");
    }
}

#[test]
fn nothing_is_found_when_there_are_no_tools_to_find() {
    let written = "<tool_call>{\"name\": \"fetch\"}</tool_call>";
    assert_eq!(text_tool_call_name(written, &[]), None);
}

#[test]
fn the_first_known_name_in_the_text_wins() {
    let written = "<tool_call>{\"name\": \"unknown_one\"}</tool_call>\n\
                   <tool_call>{\"name\": \"read\"}</tool_call>";
    assert_eq!(
        text_tool_call_name(written, &known()),
        Some("read".to_owned())
    );
}

#[test]
fn a_second_call_finds_the_same_answer_as_the_first() {
    // A regex that carried its position between calls would find nothing the
    // second time, which is the bug this guards.
    let written = "<tool_call>{\"name\": \"fetch\"}</tool_call>";
    let first = text_tool_call_name(written, &known());
    let second = text_tool_call_name(written, &known());
    assert_eq!(first, second);
    assert_eq!(second, Some("fetch".to_owned()));
}

#[test]
fn the_correction_names_the_tool_it_was_reaching_for() {
    // "Use the tool interface" is advice a model that just failed to use the
    // tool interface cannot act on.
    let correction = text_tool_call_correction("fetch");

    assert!(correction.starts_with("## Correction"));
    assert!(correction.contains("a call to `fetch` written as text"));
    assert!(correction.contains("Call `fetch` now, properly."));
    assert!(correction.contains("nothing ran"));
}
