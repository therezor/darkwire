//! Typed provider failures.
//!
//! The rule this module exists to enforce: **nothing decides what to do about
//! a failure by searching text for it.** Matching `"429"` or `"rate limit"` or
//! `"overloaded"` in a response is wrong in both directions: a model that
//! legitimately writes the words "rate limit" in its answer triggers a retry,
//! and a provider that phrases its 429 differently does not. Every decision the
//! resilience decorator makes is driven by the HTTP status and the provider's
//! own structured `error` object.
//!
//! The one concession is `code`. OpenAI-compatible endpoints return
//! `{"error": {"message", "type", "param", "code"}}`, and `code` is a
//! machine-readable enum: `context_length_exceeded`, `unsupported_parameter`.
//! Reading it is not substring sniffing; it is reading the field the protocol
//! provides for exactly this. Local servers frequently omit it, which is why a
//! bare 400 still degrades: the ladder drops parameters that were sent rather
//! than parameters that were named, so it works without the hint.
//!
//! A [`ProviderError`] is a *view* over the one [`WireError`] every crate
//! returns, not a second error type. It writes its `reason` and diagnosis into
//! `details` under fixed keys and reads them back from there, so a failure
//! crossing into the agent loop keeps a `kind` from the core taxonomy and the
//! finer `reason` survives the trip. The ladder switches on the parsed enum.

use std::error::Error as StdError;
use std::io;

use chrono::{DateTime, NaiveDateTime, Utc};
use darkwire_core::{ErrorKind, WireError};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Why a provider request failed, finer than [`ErrorKind`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorReason {
    /// 401/403. The key is missing, wrong, or lacks access to the model.
    Auth,
    /// 429, or a provider's own quota response.
    RateLimit,
    /// A parameter this model does not accept. The ladder can drop it.
    UnsupportedParam,
    /// The request exceeded the model's context window.
    ContextLength,
    /// Any other 4xx the caller has to fix.
    InvalidRequest,
    /// The model id names nothing at this endpoint.
    ModelNotFound,
    /// The provider's content filter refused the request.
    ContentFilter,
    /// 5xx.
    Server,
    /// 503/529, or an explicit "overloaded" status. Retry is the right answer.
    Overloaded,
    /// DNS, TCP, TLS: the request never reached the provider.
    Transport,
    /// The response body was not the event stream it claimed to be.
    StreamParse,
    /// A deadline passed.
    Timeout,
    /// The turn was cancelled.
    Aborted,
    /// Nothing above fits.
    Unknown,
}

impl ProviderErrorReason {
    /// Every reason, in declaration order.
    pub const ALL: [ProviderErrorReason; 14] = [
        ProviderErrorReason::Auth,
        ProviderErrorReason::RateLimit,
        ProviderErrorReason::UnsupportedParam,
        ProviderErrorReason::ContextLength,
        ProviderErrorReason::InvalidRequest,
        ProviderErrorReason::ModelNotFound,
        ProviderErrorReason::ContentFilter,
        ProviderErrorReason::Server,
        ProviderErrorReason::Overloaded,
        ProviderErrorReason::Transport,
        ProviderErrorReason::StreamParse,
        ProviderErrorReason::Timeout,
        ProviderErrorReason::Aborted,
        ProviderErrorReason::Unknown,
    ];

    /// The `snake_case` spelling, which is what `details.reason` carries.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderErrorReason::Auth => "auth",
            ProviderErrorReason::RateLimit => "rate_limit",
            ProviderErrorReason::UnsupportedParam => "unsupported_param",
            ProviderErrorReason::ContextLength => "context_length",
            ProviderErrorReason::InvalidRequest => "invalid_request",
            ProviderErrorReason::ModelNotFound => "model_not_found",
            ProviderErrorReason::ContentFilter => "content_filter",
            ProviderErrorReason::Server => "server",
            ProviderErrorReason::Overloaded => "overloaded",
            ProviderErrorReason::Transport => "transport",
            ProviderErrorReason::StreamParse => "stream_parse",
            ProviderErrorReason::Timeout => "timeout",
            ProviderErrorReason::Aborted => "aborted",
            ProviderErrorReason::Unknown => "unknown",
        }
    }

    /// Parses the `snake_case` spelling.
    pub fn parse(value: &str) -> Option<ProviderErrorReason> {
        ProviderErrorReason::ALL
            .into_iter()
            .find(|reason| reason.as_str() == value)
    }

    /// Reason to the core taxonomy kind.
    ///
    /// Mapped rather than collapsed to `provider`, so a rate limit is still
    /// `rate_limited` and a DNS failure is still `network` once the error
    /// leaves this crate: the channel layer and the UI branch on `kind`, not
    /// on `reason`.
    pub fn kind(self) -> ErrorKind {
        match self {
            ProviderErrorReason::Auth => ErrorKind::PermissionDenied,
            ProviderErrorReason::RateLimit => ErrorKind::RateLimited,
            ProviderErrorReason::ModelNotFound => ErrorKind::NotFound,
            ProviderErrorReason::Transport => ErrorKind::Network,
            ProviderErrorReason::Timeout => ErrorKind::Timeout,
            ProviderErrorReason::Aborted => ErrorKind::Aborted,
            ProviderErrorReason::UnsupportedParam
            | ProviderErrorReason::ContextLength
            | ProviderErrorReason::InvalidRequest
            | ProviderErrorReason::ContentFilter
            | ProviderErrorReason::Server
            | ProviderErrorReason::Overloaded
            | ProviderErrorReason::StreamParse
            | ProviderErrorReason::Unknown => ErrorKind::Provider,
        }
    }

    /// Whether trying the identical request again could succeed.
    ///
    /// `StreamParse` is retryable in a specific sense: not as another stream,
    /// but as a non-streaming request. `with_resilience` is what knows that
    /// distinction; here it only says the request itself was not the problem.
    pub fn default_retryable(self) -> bool {
        matches!(
            self,
            ProviderErrorReason::RateLimit
                | ProviderErrorReason::Server
                | ProviderErrorReason::Overloaded
                | ProviderErrorReason::Transport
                | ProviderErrorReason::StreamParse
                | ProviderErrorReason::Timeout
        )
    }
}

impl std::fmt::Display for ProviderErrorReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A provider failure, with the diagnosis the ladder reads.
///
/// Built with the `with_*` methods and turned into the error every crate
/// returns by [`ProviderError::into_wire`]; read back off one with
/// [`ProviderError::of`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderError {
    /// What went wrong, finer than the kind.
    pub reason: ProviderErrorReason,
    /// For humans. Never branched on.
    pub message: String,
    /// The provider the request was for. Empty when unknown.
    pub provider_id: String,
    /// The HTTP status, when there was a response.
    pub status: Option<u16>,
    /// The parameter the provider named as the problem, when it named one.
    pub param: Option<String>,
    /// The provider's machine-readable error code.
    pub code: Option<String>,
    /// From `Retry-After`, already converted to a delay.
    pub retry_after_ms: Option<i64>,
    /// Whether retrying unchanged might succeed.
    pub retryable: bool,
    /// Anything else worth logging, such as the URL.
    pub details: Map<String, Value>,
}

impl ProviderError {
    /// A failure with `reason`'s default retryability and nothing diagnosed.
    pub fn new(reason: ProviderErrorReason, message: impl Into<String>) -> ProviderError {
        ProviderError {
            reason,
            message: message.into(),
            provider_id: String::new(),
            status: None,
            param: None,
            code: None,
            retry_after_ms: None,
            retryable: reason.default_retryable(),
            details: Map::new(),
        }
    }

    /// Names the provider.
    #[must_use]
    pub fn with_provider(mut self, provider_id: impl Into<String>) -> ProviderError {
        self.provider_id = provider_id.into();
        self
    }

    /// Records the HTTP status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> ProviderError {
        self.status = Some(status);
        self
    }

    /// Records the blamed parameter, ignoring an empty one.
    #[must_use]
    pub fn with_param(mut self, param: Option<String>) -> ProviderError {
        self.param = param.filter(|value| !value.is_empty());
        self
    }

    /// Records the provider's error code, ignoring an empty one.
    #[must_use]
    pub fn with_code(mut self, code: Option<String>) -> ProviderError {
        self.code = code.filter(|value| !value.is_empty());
        self
    }

    /// Records the delay the provider asked for.
    #[must_use]
    pub fn with_retry_after_ms(mut self, delay_ms: Option<i64>) -> ProviderError {
        self.retry_after_ms = delay_ms;
        self
    }

    /// Overrides the reason's default retryability.
    #[must_use]
    pub fn with_retryable(mut self, retryable: bool) -> ProviderError {
        self.retryable = retryable;
        self
    }

    /// Adds one structured detail.
    #[must_use]
    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> ProviderError {
        self.details.insert(key.into(), value.into());
        self
    }

    /// The core error, with the diagnosis in `details` where the logger and
    /// [`ProviderError::of`] can find it. Absent fields are absent rather than
    /// present-and-null, because redaction and log filtering work by path.
    pub fn into_wire(self) -> WireError {
        let mut details = Map::new();
        details.insert("reason".into(), Value::String(self.reason.as_str().into()));
        if !self.provider_id.is_empty() {
            details.insert("providerId".into(), Value::String(self.provider_id));
        }
        if let Some(status) = self.status {
            details.insert("status".into(), Value::from(status));
        }
        if let Some(code) = self.code {
            details.insert("code".into(), Value::String(code));
        }
        if let Some(param) = self.param {
            details.insert("param".into(), Value::String(param));
        }
        if let Some(delay) = self.retry_after_ms {
            details.insert("retryAfterMs".into(), Value::from(delay));
        }
        for (key, value) in self.details {
            details.insert(key, value);
        }
        WireError::new(self.reason.kind(), self.message)
            .with_retryable(self.retryable)
            .with_details(details)
    }

    /// Reads a provider failure back off any [`WireError`].
    ///
    /// Structural rather than nominal: an error that carries a `reason` this
    /// crate wrote is that failure, whoever raised it. One that carries none
    /// is classified by its kind, the way the request path classifies what the
    /// socket threw: cancelled is `Aborted`, a deadline is `Timeout`, and
    /// everything else on that path is a failed connection.
    pub fn of(error: &WireError) -> ProviderError {
        let reason = error
            .details
            .get("reason")
            .and_then(Value::as_str)
            .and_then(ProviderErrorReason::parse);
        let reason = reason.unwrap_or(match error.kind {
            ErrorKind::Aborted => ProviderErrorReason::Aborted,
            ErrorKind::Timeout => ProviderErrorReason::Timeout,
            _ => ProviderErrorReason::Transport,
        });
        let string_field = |key: &str| {
            error
                .details
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        let mut rest = error.details.clone();
        for key in [
            "reason",
            "providerId",
            "status",
            "code",
            "param",
            "retryAfterMs",
        ] {
            rest.remove(key);
        }
        ProviderError {
            reason,
            message: error.message.clone(),
            provider_id: string_field("providerId").unwrap_or_default(),
            status: error
                .details
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok()),
            param: string_field("param"),
            code: string_field("code"),
            retry_after_ms: error.details.get("retryAfterMs").and_then(Value::as_i64),
            retryable: error.retryable,
            details: rest,
        }
    }

    /// Whether `error` carries a reason this crate wrote.
    pub fn is_provider_error(error: &WireError) -> bool {
        error
            .details
            .get("reason")
            .and_then(Value::as_str)
            .is_some_and(|reason| ProviderErrorReason::parse(reason).is_some())
    }

    /// The reason of `error`, read structurally. See [`ProviderError::of`].
    pub fn reason_of(error: &WireError) -> ProviderErrorReason {
        ProviderError::of(error).reason
    }
}

impl From<ProviderError> for WireError {
    fn from(error: ProviderError) -> WireError {
        error.into_wire()
    }
}

impl std::fmt::Display for ProviderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl StdError for ProviderError {}

/// The provider's `error` object, as far as anything here relies on it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireErrorBody {
    /// Prose. Shown, never read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// The provider's error class.
    #[serde(default, rename = "type", skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The machine-readable code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    /// The parameter blamed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
}

impl WireErrorBody {
    /// The fields this crate reads, out of a provider's `error` object.
    pub fn from_value(error: &Value) -> WireErrorBody {
        let field = |key: &str| error.get(key).and_then(Value::as_str).map(str::to_owned);
        WireErrorBody {
            message: field("message"),
            kind: field("type"),
            code: field("code"),
            param: field("param"),
        }
    }
}

/// Codes that mean the request was too long.
///
/// Exact values, not a substring scan: `context_length_exceeded` is an enum
/// member, and treating it as prose is how a model discussing context windows
/// ends up truncating its own history.
const CONTEXT_LENGTH_CODES: &[&str] = &[
    "context_length_exceeded",
    "context_window_exceeded",
    "string_above_max_length",
    "invalid_prompt_length",
];

const UNSUPPORTED_PARAM_CODES: &[&str] = &[
    "unsupported_parameter",
    "unsupported_value",
    "unknown_parameter",
    "invalid_parameter",
    "parameter_not_supported",
];

const MODEL_NOT_FOUND_CODES: &[&str] = &["model_not_found", "model_not_available", "invalid_model"];

/// HTTP status plus the structured error body to a reason.
///
/// The 400 branch carries the weight, because "the request was wrong" is the
/// only failure the ladder can actually repair. Where the provider names a
/// code or a param, that is used; where it does not (every local inference
/// server, most of the time) the reason stays `InvalidRequest` and the ladder
/// falls back to dropping whatever optional parameters the request happened
/// to carry.
pub fn classify_status(status: u16, body: Option<&WireErrorBody>) -> ProviderErrorReason {
    match status {
        401 | 403 => return ProviderErrorReason::Auth,
        404 => return ProviderErrorReason::ModelNotFound,
        408 => return ProviderErrorReason::Timeout,
        429 => return ProviderErrorReason::RateLimit,
        // 529 is Anthropic's overloaded status; it is not in the IANA registry
        // and no client library special-cases it, which is precisely why it
        // is here.
        503 | 529 => return ProviderErrorReason::Overloaded,
        500.. => return ProviderErrorReason::Server,
        _ => {}
    }

    if let Some(code) = body.and_then(|body| body.code.as_deref()) {
        if CONTEXT_LENGTH_CODES.contains(&code) {
            return ProviderErrorReason::ContextLength;
        }
        if UNSUPPORTED_PARAM_CODES.contains(&code) {
            return ProviderErrorReason::UnsupportedParam;
        }
        if MODEL_NOT_FOUND_CODES.contains(&code) {
            return ProviderErrorReason::ModelNotFound;
        }
        if code == "content_filter" {
            return ProviderErrorReason::ContentFilter;
        }
        if code == "rate_limit_exceeded" || code == "insufficient_quota" {
            return ProviderErrorReason::RateLimit;
        }
    }

    // A named parameter on a 4xx is the provider pointing at the field it
    // rejected, which is exactly what the degradation ladder needs to know.
    if body
        .and_then(|body| body.param.as_deref())
        .is_some_and(|param| !param.is_empty())
    {
        return ProviderErrorReason::UnsupportedParam;
    }

    if status >= 400 {
        ProviderErrorReason::InvalidRequest
    } else {
        ProviderErrorReason::Unknown
    }
}

/// `Retry-After` as a delay in milliseconds.
///
/// Both forms are specified, delta-seconds and an HTTP date, and providers use
/// both. `None` for anything else, so a malformed header falls back to the
/// decorator's own backoff rather than to a nonsense delay.
pub fn parse_retry_after(value: Option<&str>, now_ms: i64) -> Option<i64> {
    let trimmed = value?.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return trimmed
            .parse::<i64>()
            .ok()
            .map(|seconds| seconds.saturating_mul(1000));
    }

    // Every HTTP-date form carries a three-letter weekday and month, and
    // requiring them is what keeps a lenient date parser from accepting
    // garbage: `-5` read as a year in antiquity would clamp to a zero delay
    // and retry immediately.
    let has_word = trimmed
        .as_bytes()
        .windows(3)
        .any(|window| window.iter().all(u8::is_ascii_alphabetic));
    if !has_word {
        return None;
    }
    let date_ms = parse_http_date_ms(trimmed)?;
    Some((date_ms - now_ms).max(0))
}

/// The three HTTP-date forms, in the order the RFC lists them.
fn parse_http_date_ms(text: &str) -> Option<i64> {
    if let Ok(date) = DateTime::parse_from_rfc2822(text) {
        return Some(date.timestamp_millis());
    }
    // RFC 850 (`Sunday, 06-Nov-94 08:49:37 GMT`) and asctime
    // (`Sun Nov  6 08:49:37 1994`), both stated in GMT.
    let stripped = text.trim_end_matches(" GMT");
    ["%A, %d-%b-%y %H:%M:%S", "%a %b %e %H:%M:%S %Y"]
        .into_iter()
        .find_map(|format| NaiveDateTime::parse_from_str(stripped, format).ok())
        .map(|naive| naive.and_utc().timestamp_millis())
        .or_else(|| {
            DateTime::parse_from_rfc3339(text)
                .ok()
                .map(|date| date.with_timezone(&Utc).timestamp_millis())
        })
}

/// Where the request was aimed, so the message can name it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportContext {
    /// The full request URL. Reported as its origin; a path adds nothing here.
    pub url: Option<String>,
    /// The provider's display name. Falls back to the id.
    pub label: Option<String>,
}

/// `http://127.0.0.1:11434`, or the whole string if it does not parse.
fn origin_of(url: &str) -> String {
    match Url::parse(url) {
        Ok(parsed) if parsed.has_host() => {
            let mut origin = format!("{}://", parsed.scheme());
            origin.push_str(parsed.host_str().unwrap_or_default());
            if let Some(port) = parsed.port() {
                use std::fmt::Write as _;
                let _ = write!(origin, ":{port}");
            }
            origin
        }
        _ => url.to_owned(),
    }
}

/// What the socket said, and what an operator should do about it.
///
/// The wording names a thing to *do* wherever the failure implies one. A bare
/// "connection error" is true of every entry here and useful for none of
/// them: it does not distinguish a model server that was never started from a
/// host name that no longer resolves from a route that is gone, and those are
/// three different afternoons.
///
/// `retryable` is false where the same request is certain to fail the same way
/// until somebody changes something. It is not a claim about permanence; it
/// is what decides whether the UI offers "sending the message again may
/// work", which under a refused connection is advice to press a button that
/// cannot work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportFault {
    /// The socket's own name for what happened.
    pub code: &'static str,
    /// The half-sentence after the dash.
    pub detail: &'static str,
    /// Whether pressing send again could possibly help.
    pub retryable: bool,
}

/// The fault an I/O error kind names, if this table has wording for it.
fn fault_of_io(kind: io::ErrorKind) -> Option<TransportFault> {
    let fault = |code, detail, retryable| {
        Some(TransportFault {
            code,
            detail,
            retryable,
        })
    };
    match kind {
        io::ErrorKind::ConnectionRefused => {
            fault("ECONNREFUSED", "nothing is listening there", false)
        }
        io::ErrorKind::HostUnreachable => {
            fault("EHOSTUNREACH", "there is no route to that host", true)
        }
        io::ErrorKind::NetworkUnreachable => {
            fault("ENETUNREACH", "that network is unreachable", true)
        }
        io::ErrorKind::TimedOut => fault("ETIMEDOUT", "the connection timed out", true),
        io::ErrorKind::ConnectionReset
        | io::ErrorKind::ConnectionAborted
        | io::ErrorKind::BrokenPipe
        | io::ErrorKind::UnexpectedEof => fault(
            "ECONNRESET",
            "it closed the connection before answering",
            true,
        ),
        _ => None,
    }
}

/// The first I/O error kind in the chain, bounded.
///
/// A source chain is a linked list a library controls, and one that loops
/// would hang the error path; five hops is deeper than any real one.
fn io_kind_in(error: &(dyn StdError + 'static)) -> Option<io::ErrorKind> {
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    for _ in 0..5 {
        let error = current?;
        if let Some(io) = error.downcast_ref::<io::Error>() {
            return Some(io.kind());
        }
        current = error.source();
    }
    None
}

/// Normalises a connection failure into a `transport` error that names the
/// endpoint and says what happened.
///
/// This is where the message is *built* rather than forwarded, and it is the
/// one case where forwarding is wrong: the HTTP client's own message is the
/// same for every connection failure there is.
pub fn transport_error(
    error: &(dyn StdError + 'static),
    provider_id: &str,
    context: &TransportContext,
) -> ProviderError {
    let name = context
        .label
        .clone()
        .unwrap_or_else(|| provider_id.to_owned());
    let target = match &context.url {
        Some(url) => format!("{name} at {}", origin_of(url)),
        None => name,
    };
    let fault = io_kind_in(error).and_then(fault_of_io);
    let detail = match fault {
        Some(fault) => fault.detail.to_owned(),
        // No kind this table has wording for: the client's own message, which
        // is at least specific when it is not a bare "error sending request".
        None => innermost_message(error),
    };

    let mut result = ProviderError::new(
        ProviderErrorReason::Transport,
        format!("Could not reach {target}: {detail}."),
    )
    .with_provider(provider_id);
    if let Some(fault) = fault {
        result = result
            .with_retryable(fault.retryable)
            .with_detail("code", fault.code);
    }
    if let Some(url) = &context.url {
        result = result.with_detail("url", url.as_str());
    }
    result
}

/// The deepest message in the chain, which is where the specific one lives.
fn innermost_message(error: &(dyn StdError + 'static)) -> String {
    let mut current: &(dyn StdError + 'static) = error;
    for _ in 0..5 {
        match current.source() {
            Some(next) => current = next,
            None => break,
        }
    }
    current.to_string()
}

/// Everything the HTTP client can raise on the request path, typed.
///
/// A deadline the client itself enforced is a `timeout`; anything else is a
/// failed connection, worded by [`transport_error`].
pub fn to_provider_error(
    error: &reqwest::Error,
    provider_id: &str,
    context: &TransportContext,
) -> ProviderError {
    if error.is_timeout() {
        return ProviderError::new(ProviderErrorReason::Timeout, "Request timed out")
            .with_provider(provider_id);
    }
    let mut result = transport_error(error, provider_id, context);
    if error.is_body() || error.is_decode() {
        // Headers arrived and the body did not finish: the connection worked
        // and then stopped, which is the retryable shape.
        result.retryable = true;
    }
    result
}
