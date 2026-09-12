//! The credential leaves and the patch request, which carry rules the
//! fixtures cannot show.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use garde::Validate;
use ghostai_protocol::{
    DEFAULT_USERNAME, LoginRequest, NewPassword, PASSWORD_MAX_LENGTH, PASSWORD_MIN_LENGTH,
    PresentedPassword, SettingsPatchRequest, USERNAME_MAX_LENGTH, USERNAME_MIN_LENGTH,
    USERNAME_PATTERN, Username,
};
use serde_json::json;

#[test]
fn a_username_is_trimmed_and_folded_on_the_way_in() {
    let request: LoginRequest =
        serde_json::from_value(json!({"username": "  Ghost ", "password": "hunter2hunter2"}))
            .unwrap();
    assert_eq!(request.username.as_str(), "ghost");
    assert_eq!(request.username, Username::new(DEFAULT_USERNAME));
    assert!(request.validate().is_ok());
    assert_eq!(
        serde_json::to_value(&request.username).unwrap(),
        json!("ghost")
    );
}

#[test]
fn the_bounds_are_the_published_constants() {
    assert_eq!((USERNAME_MIN_LENGTH, USERNAME_MAX_LENGTH), (1, 64));
    assert_eq!((PASSWORD_MIN_LENGTH, PASSWORD_MAX_LENGTH), (12, 256));
    assert_eq!(USERNAME_PATTERN, "^[a-z0-9][a-z0-9._-]*$");
    assert!(Username::new("").validate().is_err());
    assert!(Username::new("-x").validate().is_err());
    assert!(Username::new(&"a".repeat(65)).validate().is_err());
    assert!(Username::new("a.b-c_9").validate().is_ok());
    assert!(NewPassword("short".into()).validate().is_err());
    assert!(NewPassword("x".repeat(12)).validate().is_ok());
    assert!(NewPassword("x".repeat(257)).validate().is_err());
    assert!(PresentedPassword(String::new()).validate().is_err());
    assert!(PresentedPassword("x".into()).validate().is_ok());
}

#[test]
fn a_settings_patch_refuses_a_section_this_build_does_not_have() {
    let stale = json!({"agents": {"defaults": {"model": "x"}}});
    assert!(serde_json::from_value::<SettingsPatchRequest>(stale).is_err());
    let unknown = json!({"nope": {}});
    assert!(serde_json::from_value::<SettingsPatchRequest>(unknown).is_err());
}

#[test]
fn a_settings_patch_splits_into_the_patch_and_the_renames() {
    let request: SettingsPatchRequest = serde_json::from_value(json!({
        "ui": {"locale": "uk"},
        "renameAgents": [{"from": "reviewer", "to": "code-review"}],
    }))
    .unwrap();
    let (patch, renames) = request.into_parts();
    assert_eq!(patch.ui.unwrap().locale.as_deref(), Some("uk"));
    assert_eq!(renames.len(), 1);
    assert_eq!(renames[0].to, "code-review");
    let (_, none) = SettingsPatchRequest::default().into_parts();
    assert!(none.is_empty());
}
