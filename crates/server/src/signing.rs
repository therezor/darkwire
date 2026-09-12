//! HMAC-signed, expiring media URLs.
//!
//! `<img src>` cannot carry an `Authorization` header and will not send a
//! `SameSite=Strict` cookie on every path a browser might load it from, so an
//! authenticated file endpoint cannot be rendered inline. The tempting fix —
//! make the file endpoint public — is anonymous read access to everything under
//! the workspace, which is the agent's whole filesystem.
//!
//! A signature satisfies the browser instead. The *URL* is the credential, it
//! names one path, it expires, and the endpoint that serves it stays outside
//! the session-cookie surface rather than outside authorisation.
//!
//! Three properties this module exists to hold:
//!
//!  - **The path is inside the signature, not beside it.** A token that
//!    authorised "some file" plus a `?path=` parameter is a token that
//!    authorises every file.
//!  - **The comparison is constant-time.** A byte-at-a-time `==` on a MAC is
//!    forgeable given enough attempts, and a media URL is exactly the kind of
//!    thing something retries in a loop.
//!  - **Expiry is checked after the MAC verifies**, so an unsigned guess learns
//!    nothing from the difference between "expired" and "wrong".

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ghostai_core::{ErrorKind, GhostError, Result};
use hmac::{Hmac, KeyInit as _, Mac as _};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq as _;

/// The `auth_secrets` row the signing key lives in.
pub const MEDIA_SECRET_NAME: &str = "media_signing_key";

/// What a verified token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaClaim {
    /// Workspace-relative, as it was when signed.
    pub path: String,
    /// Which workspace the path is relative to.
    ///
    /// Inside the signature for exactly the reason the path is: two workspaces
    /// both contain `notes.md`, so a token that authorised "this path" without
    /// saying where would be a token for that filename in every workspace at
    /// once.
    pub workspace_id: String,
    /// When the token stops being accepted.
    pub expires_at_ms: i64,
}

/// The signed payload, in the field order the encoding depends on.
///
/// The MAC covers the encoded bytes, so the order these serialise in is part of
/// the wire format rather than a presentation detail: a token minted with the
/// fields in another order would not verify against one minted here.
#[derive(Debug, Serialize, Deserialize)]
struct Payload {
    /// The workspace-relative path.
    p: String,
    /// The workspace the path belongs to.
    w: String,
    /// Expiry, in epoch milliseconds.
    e: i64,
}

/// The MAC over an encoded payload.
///
/// `new_from_slice` is infallible for HMAC — any key length is accepted, since
/// the construction hashes an over-long key and zero-pads a short one — so the
/// only way to reach the fallback is a type change, and an empty MAC would
/// never match a real one.
fn mac(secret: &str, encoded: &str) -> Vec<u8> {
    match Hmac::<Sha256>::new_from_slice(secret.as_bytes()) {
        Ok(mut hmac) => {
            hmac.update(encoded.as_bytes());
            hmac.finalize().into_bytes().to_vec()
        }
        Err(_) => Vec::new(),
    }
}

/// A token for one path, good until `expires_at_ms`.
///
/// The caller has already put the path through the jail — signing an escaping
/// path would make the signature the thing that authorised it.
pub fn sign_media_token(secret: &str, claim: &MediaClaim) -> String {
    let payload = Payload {
        p: claim.path.clone(),
        w: claim.workspace_id.clone(),
        e: claim.expires_at_ms,
    };
    // The payload is three owned strings and an integer, so the only way
    // serialisation fails is a non-finite number, which this shape cannot hold.
    let json = serde_json::to_vec(&payload).unwrap_or_default();
    let encoded = URL_SAFE_NO_PAD.encode(json);
    let signature = URL_SAFE_NO_PAD.encode(mac(secret, &encoded));
    format!("{encoded}.{signature}")
}

/// The claim a token carries, or `None` for anything that is not a token this
/// server signed and that is still live.
///
/// One answer for every failure — malformed, forged, expired — because
/// distinguishing them tells a caller which half of a guess was right.
pub fn verify_media_token(secret: &str, token: &str, now_ms: i64) -> Option<MediaClaim> {
    let separator = token.rfind('.')?;
    if separator == 0 || separator == token.len() - 1 {
        return None;
    }
    let encoded = token.get(..separator)?;
    let presented = URL_SAFE_NO_PAD.decode(token.get(separator + 1..)?).ok()?;

    // The MAC first, and only then the payload: a forged token must not reach
    // the JSON parser, and an expired one must not be distinguishable from a
    // forged one by how far it got.
    let expected = mac(secret, encoded);
    if presented.len() != expected.len() {
        return None;
    }
    if !bool::from(presented.ct_eq(&expected)) {
        return None;
    }

    // Unreachable through a signature this server produced; reachable if the
    // signing key ever leaked, which is exactly when not failing loudly matters.
    let decoded = URL_SAFE_NO_PAD.decode(encoded).ok()?;
    let payload: Payload = serde_json::from_slice(&decoded).ok()?;

    if payload.p.is_empty() {
        return None;
    }
    // A workspace-less payload is refused rather than defaulted to `default`.
    // Defaulting would let a token minted before workspaces existed — or one
    // whose `w` an attacker stripped — be replayed against the default
    // workspace, which is the one that contains all the others.
    if payload.w.is_empty() {
        return None;
    }
    if payload.e <= now_ms {
        return None;
    }

    Some(MediaClaim {
        path: payload.p,
        workspace_id: payload.w,
        expires_at_ms: payload.e,
    })
}

/// The URL a client puts in `<img src>`. Relative, so it survives a reverse
/// proxy.
pub fn media_url(token: &str) -> String {
    format!("/api/media/{}", encode_uri_component(token))
}

/// Percent-encoding with the unreserved set a browser's own encoder uses.
///
/// base64url never produces a character this escapes, but a token is a
/// credential and the encoder is what stops one from being reinterpreted as a
/// path segment.
fn encode_uri_component(value: &str) -> String {
    use std::fmt::Write as _;

    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&byte) {
            out.push(char::from(byte));
        } else {
            // Writing into a `String` cannot fail; the result is discarded
            // rather than unwrapped so this stays panic-free.
            let _ = write!(out, "%{byte:02X}");
        }
    }
    out
}

/// Guards against a signer built with no key, which would sign everything
/// alike.
pub fn assert_signing_key(secret: &str) -> Result<()> {
    if secret.is_empty() {
        return Err(GhostError::new(
            ErrorKind::Config,
            "The media signing key is empty",
        ));
    }
    Ok(())
}
