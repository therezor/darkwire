//! HMAC-signed media tokens: what they authorise, and what they refuse.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ghostai_core::ErrorKind;
use ghostai_server::signing::{
    MediaClaim, assert_signing_key, media_url, sign_media_token, verify_media_token,
};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde_json::json;
use sha2::Sha256;

const KEY: &str = "a-signing-key";
const NOW: i64 = 1_700_000_000_000;

fn claim(path: &str, workspace_id: &str, expires_at_ms: i64) -> MediaClaim {
    MediaClaim {
        path: path.to_owned(),
        workspace_id: workspace_id.to_owned(),
        expires_at_ms,
    }
}

fn token() -> String {
    sign_media_token(KEY, &claim("notes/photo.png", "default", NOW + 60_000))
}

/// A token assembled by hand, the way an attacker would: an arbitrary payload
/// with a signature computed over it, or over something else.
fn hand_signed(payload: &serde_json::Value, key: &str) -> String {
    let encoded = URL_SAFE_NO_PAD.encode(serde_json::to_vec(payload).unwrap());
    let mut mac = Hmac::<Sha256>::new_from_slice(key.as_bytes()).unwrap();
    mac.update(encoded.as_bytes());
    let signature = URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    format!("{encoded}.{signature}")
}

#[test]
fn a_token_it_signed_verifies() {
    assert_eq!(
        verify_media_token(KEY, &token(), NOW),
        Some(claim("notes/photo.png", "default", NOW + 60_000))
    );
}

#[test]
fn a_token_signed_with_another_key_is_refused() {
    assert_eq!(verify_media_token("another-key", &token(), NOW), None);
}

#[test]
fn expiry_is_a_deadline_not_a_grace_period() {
    let expiring = sign_media_token(KEY, &claim("notes/photo.png", "default", NOW));
    assert!(verify_media_token(KEY, &expiring, NOW - 1).is_some());
    // Exactly at the boundary counts as expired: a token is good *until* its
    // deadline, and "still valid at the instant it ran out" is the reading that
    // makes a lifetime one millisecond longer than it says.
    assert_eq!(verify_media_token(KEY, &expiring, NOW), None);
}

/// The whole point of putting the path inside the MAC: a token authorises one
/// file, not "some file plus whatever the query string says".
#[test]
fn a_token_whose_path_was_swapped_is_refused() {
    let original = token();
    let signature = original.rsplit('.').next().unwrap();
    let edited = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({"p": "../../etc/passwd", "w": "default", "e": NOW + 60_000}))
            .unwrap(),
    );
    assert_eq!(
        verify_media_token(KEY, &format!("{edited}.{signature}"), NOW),
        None
    );
}

#[test]
fn a_malformed_token_is_refused_rather_than_failing() {
    let truncated = {
        let full = token();
        full.get(..full.len() - 4).unwrap().to_owned()
    };
    for value in [
        String::new(),
        "justonepart".to_owned(),
        ".signature".to_owned(),
        "payload.".to_owned(),
        truncated,
        // A signature half that is not base64url at all: the decoder is strict,
        // and a scanner sending this must get the same `None` as a forgery.
        "payload.!!!!".to_owned(),
    ] {
        assert_eq!(verify_media_token(KEY, &value, NOW), None, "{value:?}");
    }
}

#[test]
fn a_correctly_signed_payload_that_is_not_the_right_shape_is_refused() {
    // Signed with the real key, so the MAC passes and the refusal comes from
    // the payload check rather than from the comparison.
    for payload in [
        json!({"p": "", "w": "default", "e": NOW + 60_000}),
        // No workspace: defaulting it would let a token minted before
        // workspaces existed be replayed against `default`, which is the
        // workspace that contains all the others.
        json!({"p": "notes/photo.png", "e": NOW + 60_000}),
        json!({"p": "notes/photo.png", "w": "", "e": NOW + 60_000}),
        json!({"w": "default", "e": NOW + 60_000}),
        json!({"p": "notes/photo.png", "w": "default"}),
        json!("not an object"),
    ] {
        assert_eq!(
            verify_media_token(KEY, &hand_signed(&payload, KEY), NOW),
            None,
            "{payload}"
        );
    }
}

#[test]
fn a_tampered_workspace_is_refused_because_the_mac_covers_it() {
    // The signature from a *different* payload, which is what tampering leaves.
    let stale = sign_media_token(KEY, &claim("notes/photo.png", "research", NOW + 60_000));
    let stale_signature = stale.rsplit('.').next().unwrap();
    let forged = URL_SAFE_NO_PAD.encode(
        serde_json::to_vec(&json!({"p": "notes/photo.png", "w": "acme", "e": NOW + 60_000}))
            .unwrap(),
    );
    assert_eq!(
        verify_media_token(KEY, &format!("{forged}.{stale_signature}"), NOW),
        None
    );
}

#[test]
fn the_workspace_is_inside_the_signature_so_two_workspaces_are_two_tokens() {
    let acme = sign_media_token(KEY, &claim("notes.md", "acme", NOW + 60_000));
    let research = sign_media_token(KEY, &claim("notes.md", "research", NOW + 60_000));
    assert_eq!(
        verify_media_token(KEY, &acme, NOW).map(|c| c.workspace_id),
        Some("acme".to_owned())
    );
    assert_ne!(
        verify_media_token(KEY, &acme, NOW),
        verify_media_token(KEY, &research, NOW)
    );
}

#[test]
fn the_url_is_relative_so_a_reverse_proxy_does_not_have_to_be_told_about_it() {
    assert_eq!(media_url("abc.def"), "/api/media/abc.def");
}

#[test]
fn a_token_is_percent_encoded_into_the_url() {
    // base64url never produces these, but a token is a credential and the
    // encoder is what stops one from being reinterpreted as a path.
    assert_eq!(media_url("a/b.c"), "/api/media/a%2Fb.c");
    assert_eq!(media_url("a b"), "/api/media/a%20b");
    assert_eq!(media_url("a-_.~*"), "/api/media/a-_.~*");
}

#[test]
fn an_empty_signing_key_is_refused() {
    let error = assert_signing_key("").unwrap_err();
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(error.message.contains("empty"));
    assert!(assert_signing_key(KEY).is_ok());
}
