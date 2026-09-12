//! The SSE reader, against `fixtures/sse/frames.json`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use futures::StreamExt;
use ghostai_core::GhostError;
use ghostai_providers::{
    MAX_SSE_FRAME_CHARS, ProviderError, ProviderErrorReason, SseEvent, SseOptions, SseParser,
    parse_sse, parse_sse_chunks,
};
use proptest::prelude::*;
use serde_json::{Value, json};

fn events(chunks: &[&str]) -> Vec<SseEvent> {
    parse_sse_chunks(chunks.iter().map(|c| c.as_bytes()), SseOptions::default()).unwrap()
}

fn event(data: &str) -> SseEvent {
    SseEvent {
        event: "message".into(),
        data: data.into(),
    }
}

#[test]
fn joins_multiple_data_lines_and_strips_one_space() {
    assert_eq!(events(&["data: hello\n\n"]), vec![event("hello")]);
    // Keeping only the last line is the classic bug here.
    assert_eq!(
        events(&["data: one\ndata: two\n\n"]),
        vec![event("one\ntwo")]
    );
    assert_eq!(events(&["data:  indented\n\n"]), vec![event(" indented")]);
    assert_eq!(events(&["data:none\n\n"]), vec![event("none")]);
}

#[test]
fn accepts_every_terminator_and_resets_the_event_name() {
    assert_eq!(events(&["data: a\r\n\r\n"]), vec![event("a")]);
    assert_eq!(events(&["data: b\r\r"]), vec![event("b")]);
    assert_eq!(
        events(&["event: ping\ndata: 1\n\ndata: 2\n\n"]),
        vec![
            SseEvent {
                event: "ping".into(),
                data: "1".into()
            },
            event("2")
        ]
    );
    assert_eq!(
        events(&[": keep-alive\n\nid: 7\nretry: 100\ndata: x\n\n"]),
        vec![event("x")]
    );
    assert_eq!(events(&["\n\n\n"]), Vec::<SseEvent>::new());
}

#[test]
fn reassembles_frames_and_characters_split_across_chunks() {
    assert_eq!(
        events(&["da", "ta: hel", "lo\n", "\ndata: more\n\n"]),
        vec![event("hello"), event("more")]
    );
    let bytes = "data: café\n\n".as_bytes();
    let split = bytes.iter().position(|b| *b == 0xc3).unwrap() + 1;
    let parsed =
        parse_sse_chunks([&bytes[..split], &bytes[split..]], SseOptions::default()).unwrap();
    assert_eq!(parsed, vec![event("café")]);
    // A final frame without its blank line is still a frame.
    assert_eq!(events(&["data: [DONE]"]), vec![event("[DONE]")]);
    // An incomplete sequence at the very end is malformed, not pending.
    let truncated = parse_sse_chunks([&b"data: caf\xc3"[..]], SseOptions::default()).unwrap();
    assert_eq!(truncated, vec![event("caf\u{FFFD}")]);
}

#[test]
fn refuses_a_frame_that_never_terminates() {
    let flood = format!("data: {}", "x".repeat(MAX_SSE_FRAME_CHARS + 1));
    let error = parse_sse_chunks([flood.as_bytes()], SseOptions::default()).unwrap_err();
    assert_eq!(
        ProviderError::reason_of(&error),
        ProviderErrorReason::StreamParse
    );

    let source = format!("data: {}", "x".repeat(200));
    let lowered = parse_sse_chunks(
        [source.as_bytes()],
        SseOptions {
            provider_id: Some("test".into()),
            max_frame_chars: Some(64),
        },
    )
    .unwrap_err();
    assert!(
        lowered.message.contains("exceeded 64 characters"),
        "{}",
        lowered.message
    );
    assert_eq!(ProviderError::of(&lowered).provider_id, "test");
}

#[test]
fn the_cap_counts_utf16_units() {
    // Four astral characters are eight UTF-16 units: over a cap of seven,
    // under a cap of eight, whatever their byte length.
    let frame = "data: 😀😀😀😀";
    assert!(
        parse_sse_chunks(
            [frame.as_bytes()],
            SseOptions {
                provider_id: None,
                max_frame_chars: Some(13)
            }
        )
        .is_err()
    );
    assert!(
        parse_sse_chunks(
            [frame.as_bytes()],
            SseOptions {
                provider_id: None,
                max_frame_chars: Some(14)
            }
        )
        .is_ok()
    );
}

#[test]
fn the_fixture_pins_every_case() {
    let fixture = common::read_fixture("sse/frames.json");
    let cases = common::cases(&fixture);
    assert_eq!(cases.len(), 22);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let chunks: Vec<Vec<u8>> = case["input"]["chunks"]
            .as_array()
            .unwrap()
            .iter()
            .map(|chunk| match chunk {
                Value::String(text) => text.as_bytes().to_vec(),
                other => hex(other["hex"].as_str().unwrap()),
            })
            .collect();
        let options = SseOptions {
            provider_id: None,
            max_frame_chars: case["input"]["maxFrameChars"]
                .as_u64()
                .and_then(|n| usize::try_from(n).ok()),
        };
        let result = parse_sse_chunks(chunks.iter().map(Vec::as_slice), options);
        let expected = &case["output"];
        if let Some(error) = expected.get("error") {
            let failure = result.unwrap_err_with(name);
            assert_eq!(
                ProviderError::reason_of(&failure).as_str(),
                error["reason"].as_str().unwrap(),
                "{name}"
            );
        } else {
            let got =
                serde_json::to_value(result.unwrap_or_else(|e| panic!("{name}: {e}"))).unwrap();
            assert_eq!(got, expected["events"], "{name}");
        }
    }
}

trait UnwrapErrWith<T> {
    fn unwrap_err_with(self, name: &str) -> GhostError;
}

impl<T: std::fmt::Debug> UnwrapErrWith<T> for Result<T, GhostError> {
    fn unwrap_err_with(self, name: &str) -> GhostError {
        match self {
            Ok(value) => panic!("{name}: expected an error, got {value:?}"),
            Err(error) => error,
        }
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[tokio::test]
async fn parse_sse_streams_frames_and_ends_on_the_first_error() {
    let source = futures::stream::iter(vec![
        Ok::<Vec<u8>, GhostError>(b"data: a\n\nda".to_vec()),
        Ok(b"ta: b\n\n".to_vec()),
        Ok(b"data: trailing".to_vec()),
    ]);
    let parsed: Vec<SseEvent> = parse_sse(source, SseOptions::default())
        .map(|event| event.unwrap())
        .collect()
        .await;
    assert_eq!(parsed, vec![event("a"), event("b"), event("trailing")]);

    let failing = futures::stream::iter(vec![
        Ok::<Vec<u8>, GhostError>(b"data: first\n\n".to_vec()),
        Err(GhostError::aborted("read")),
        Ok(b"data: never\n\n".to_vec()),
    ]);
    let results: Vec<Result<SseEvent, GhostError>> =
        parse_sse(failing, SseOptions::default()).collect().await;
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].as_ref().unwrap(), &event("first"));
    assert!(results[1].as_ref().unwrap_err().is_aborted());

    let flooding = futures::stream::iter(vec![Ok::<Vec<u8>, GhostError>(
        format!("data: {}", "x".repeat(100)).into_bytes(),
    )]);
    let results: Vec<Result<SseEvent, GhostError>> = parse_sse(
        flooding,
        SseOptions {
            provider_id: None,
            max_frame_chars: Some(10),
        },
    )
    .collect()
    .await;
    assert_eq!(results.len(), 1);
    assert_eq!(
        ProviderError::reason_of(results[0].as_ref().unwrap_err()),
        ProviderErrorReason::StreamParse
    );
}

#[test]
fn the_parser_is_debuggable_and_serialises_events() {
    let parser = SseParser::new(SseOptions::default());
    assert!(format!("{parser:?}").contains("SseParser"));
    assert_eq!(
        serde_json::to_value(event("x")).unwrap(),
        json!({"event": "message", "data": "x"})
    );
}

proptest! {
    #[test]
    fn never_loses_or_reorders_data_however_the_bytes_are_cut(
        payloads in prop::collection::vec("[a-z0-9 ]{1,20}", 1..6),
        cuts in prop::collection::vec(1usize..12, 1..8),
    ) {
        // Framing is a function of the byte sequence, not of how the
        // transport happened to chunk it.
        let text: String = payloads.iter().fold(String::new(), |acc, p| acc + &format!("data: {p}\n\n"));
        let bytes = text.as_bytes();
        let mut parts: Vec<&[u8]> = Vec::new();
        let mut offset = 0;
        let mut index = 0;
        while offset < bytes.len() {
            let size = cuts[index % cuts.len()];
            let end = (offset + size).min(bytes.len());
            parts.push(&bytes[offset..end]);
            offset = end;
            index += 1;
        }
        let parsed = parse_sse_chunks(parts, SseOptions::default()).unwrap();
        let data: Vec<String> = parsed.into_iter().map(|e| e.data).collect();
        prop_assert_eq!(data, payloads);
    }
}
