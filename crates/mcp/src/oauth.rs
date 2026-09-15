//! The OAuth flow for one MCP server, backed by the credential vault.
//!
//! Written here on `oauth2` rather than taken from the SDK: RFC 8414
//! discovery, RFC 7591 dynamic client registration, the PKCE S256 code
//! exchange and the refresh grant, in that order of use. Three decisions about
//! storage, each with a different answer:
//!
//! - **Tokens and dynamic client registration go to the vault.** They are
//!   long-lived credentials this process obtained, not settings a person
//!   typed, which is precisely the line the vault exists to hold.
//! - **The PKCE verifier is memory-only.** It is valid for one exchange, over
//!   in seconds; persisting it would leave a credential on disk with no
//!   remaining purpose, and it is meaningless across a restart anyway.
//! - **Nothing opens a browser.** [`OAuthFlow::begin_authorization`] hands the
//!   URL to the manager, which puts the server in `needs_authorization` with a
//!   link on it. A headless server that shells out to `open` is a headless
//!   server that fails in a way nobody can see.
//!
//! ## Which endpoints are guarded, and why only those
//!
//! The MCP `url` itself is operator configuration and is not run through the
//! SSRF guard (see [`crate::spec`]). The endpoints *discovery* returns are a
//! different matter: a server can name any host as its authorization server,
//! and what this client then posts to that host is a credential. So a
//! discovered endpoint is checked against the network policy when the flow is
//! given one, while the `authUrl`/`tokenUrl` the operator typed — the fallback
//! when discovery fails — are trusted the way the MCP `url` is.
//!
//! GhostAI registers as a **public client**: it runs on the operator's own
//! machine and has nowhere to keep a client secret the operator cannot already
//! read. PKCE is what stands in for one, which is what it is for.

use std::sync::Arc;

use base64::Engine as _;
use ghostai_core::{Clock, ErrorKind, GhostError, Result};
use ghostai_protocol::McpOAuthConfig;
use ghostai_security::{DnsResolver, NetworkPolicy, RandomSource, validate_target};
use oauth2::basic::BasicClient;
use oauth2::url::Url;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointNotSet,
    EndpointSet, HttpRequest, HttpResponse, PkceCodeChallenge, PkceCodeVerifier, RedirectUrl,
    RefreshToken, RequestTokenError, Scope, TokenResponse as _, TokenUrl,
};
use serde::{Deserialize, Serialize};

use crate::store::{McpSecretSlot, McpSecretStore};

/// How GhostAI describes itself to an authorization server.
pub const OAUTH_CLIENT_NAME: &str = "GhostAI";

/// A token whose expiry is this close is refreshed before it is used, so a
/// call that starts just under the line does not fail just over it.
const EXPIRY_SKEW_MS: i64 = 60_000;

/// Bytes of entropy behind a PKCE verifier; 32 becomes 43 URL-safe characters.
const VERIFIER_BYTES: usize = 32;

/// What RFC 7591 registration sends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientMetadata {
    /// The human name the authorization server shows on its consent page.
    pub client_name: String,
    /// Where codes come back.
    pub redirect_uris: Vec<String>,
    /// `authorization_code` and `refresh_token`.
    pub grant_types: Vec<String>,
    /// `code`.
    pub response_types: Vec<String>,
    /// `none`: a public client.
    pub token_endpoint_auth_method: String,
    /// The configured scopes, space-separated. Absent when none are.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// The identity an authorization server knows this client by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientInformation {
    /// Issued by registration, or the configured `clientId`.
    pub client_id: String,
    /// Issued by registration for a server that insists on one. Sent in the
    /// request body; never operator-typed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

/// What the vault holds for a server that has authorized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredTokens {
    /// Sent as `Authorization: Bearer`.
    pub access_token: String,
    /// The token type the server named, usually `Bearer`.
    pub token_type: String,
    /// Absent for a server that issued none; the flow restarts when the access
    /// token expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// Wall-clock expiry from the injected clock. Absent means "not stated".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<i64>,
    /// The scopes the server granted, space-separated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
}

/// What discovery found, or what the config supplied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    /// Where the operator is sent.
    pub authorization: Url,
    /// Where codes and refresh tokens are exchanged.
    pub token: Url,
    /// Where a client registers, when the server offers it.
    pub registration: Option<Url>,
    /// Whether these came from discovery rather than the config; only
    /// discovered endpoints are guarded.
    pub discovered: bool,
}

/// Which stored credential the server declared no longer good.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidationScope {
    /// Tokens, client and verifier alike.
    All,
    /// The registered client.
    Client,
    /// The access and refresh tokens.
    Tokens,
    /// The outstanding PKCE verifier.
    Verifier,
    /// Cached discovery; the next attempt rediscovers.
    Discovery,
}

/// The network policy a discovered endpoint must satisfy.
pub struct EndpointGuard {
    /// Which hosts and ranges are acceptable.
    pub policy: NetworkPolicy,
    /// Resolves the endpoint's host for classification.
    pub resolver: Arc<dyn DnsResolver>,
}

impl std::fmt::Debug for EndpointGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EndpointGuard")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

/// Everything one flow needs.
pub struct OAuthFlowOptions {
    /// The server this flow authorizes.
    pub server_id: String,
    /// Its OAuth block from the config.
    pub config: McpOAuthConfig,
    /// Where tokens and the registered client go.
    pub store: Arc<dyn McpSecretStore>,
    /// The loopback callback, from [`crate::callback::CallbackListener`].
    pub redirect_url: String,
    /// The `state` minted for this attempt, so the callback can route it.
    pub state: String,
    /// Speaks to the authorization server. Not the guarded fetch: the guard's
    /// redirect and pinning machinery is for URLs a model chose, and these
    /// endpoints are checked once, up front, by [`EndpointGuard`].
    pub http: reqwest::Client,
    /// Mints the PKCE verifier.
    pub random: Arc<dyn RandomSource>,
    /// Decides whether a token is still good.
    pub clock: Arc<dyn Clock>,
    /// The policy discovered endpoints must pass. `None` trusts discovery,
    /// which is only right for a test.
    pub guard: Option<Arc<EndpointGuard>>,
    /// Called with the URL the operator has to visit. Never opens anything.
    pub on_authorization_required: Arc<dyn Fn(&str) + Send + Sync>,
}

/// One server's OAuth session.
pub struct OAuthFlow {
    options: OAuthFlowOptions,
    verifier: parking_lot::Mutex<Option<String>>,
    endpoints: tokio::sync::Mutex<Option<Endpoints>>,
}

impl std::fmt::Debug for OAuthFlow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthFlow")
            .field("server_id", &self.options.server_id)
            .field("redirect_url", &self.options.redirect_url)
            .finish_non_exhaustive()
    }
}

/// A vault entry that does not parse is a credential from an older shape or a
/// corrupted write. Treating it as absent restarts the flow, which is
/// recoverable; failing here would wedge the server permanently.
fn read_json<T: for<'de> Deserialize<'de>>(raw: Option<String>) -> Option<T> {
    raw.and_then(|text| serde_json::from_str(&text).ok())
}

fn write_json<T: Serialize>(
    store: &dyn McpSecretStore,
    server_id: &str,
    slot: McpSecretSlot,
    value: &T,
) -> Result<()> {
    let text = serde_json::to_string(value).map_err(|error| {
        GhostError::new(
            ErrorKind::Internal,
            "An OAuth record could not be serialised",
        )
        .with_source(error)
    })?;
    store.write(server_id, slot, &text)
}

/// The error the HTTP adapter hands `oauth2`.
#[derive(Debug)]
struct HttpError(String);

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HttpError {}

/// One request through the workspace's `reqwest`, in the shape `oauth2` wants.
async fn send(
    client: reqwest::Client,
    request: HttpRequest,
) -> std::result::Result<HttpResponse, HttpError> {
    let (parts, body) = request.into_parts();
    let url = Url::parse(&parts.uri.to_string())
        .map_err(|error| HttpError(format!("token endpoint is not a URL: {error}")))?;
    let response = client
        .request(parts.method, url)
        .headers(parts.headers)
        .body(body)
        .send()
        .await
        .map_err(|error| HttpError(error.to_string()))?;
    let mut builder = oauth2::http::Response::builder().status(response.status());
    if let Some(headers) = builder.headers_mut() {
        headers.extend(response.headers().clone());
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| HttpError(error.to_string()))?;
    builder
        .body(bytes.to_vec())
        .map_err(|error| HttpError(error.to_string()))
}

/// The workspace's `reqwest` in the shape `oauth2` drives, with a boxed
/// `Send` future so the flow itself stays sendable across tasks.
struct HttpAdapter(reqwest::Client);

impl<'c> oauth2::AsyncHttpClient<'c> for HttpAdapter {
    type Error = HttpError;
    type Future = futures::future::BoxFuture<'c, std::result::Result<HttpResponse, HttpError>>;

    fn call(&'c self, request: HttpRequest) -> Self::Future {
        Box::pin(send(self.0.clone(), request))
    }
}

type ConfiguredClient =
    BasicClient<EndpointSet, EndpointNotSet, EndpointNotSet, EndpointNotSet, EndpointSet>;

/// RFC 8414 authorization server metadata, the three fields this flow reads.
#[derive(Debug, Deserialize)]
struct ServerMetadata {
    #[serde(rename = "authorization_endpoint")]
    authorization: String,
    #[serde(rename = "token_endpoint")]
    token: String,
    #[serde(default, rename = "registration_endpoint")]
    registration: Option<String>,
}

/// RFC 9728 protected resource metadata, the one field this flow reads.
#[derive(Debug, Deserialize)]
struct ResourceMetadata {
    #[serde(default)]
    authorization_servers: Vec<String>,
}

/// RFC 7591's answer.
#[derive(Debug, Deserialize)]
struct RegistrationResponse {
    client_id: String,
    #[serde(default)]
    client_secret: Option<String>,
}

fn oauth_error(server_id: &str, message: impl Into<String>) -> GhostError {
    GhostError::new(ErrorKind::PermissionDenied, message.into()).with_detail("server", server_id)
}

/// `RequestTokenError` as the taxonomy: a server that answered is
/// `permission_denied`, one that did not is `network`.
fn token_error<T: oauth2::ErrorResponse + 'static>(
    server_id: &str,
    what: &str,
    error: RequestTokenError<HttpError, T>,
) -> GhostError {
    match error {
        RequestTokenError::ServerResponse(response) => {
            oauth_error(server_id, format!("{what} was refused: {response}"))
                .with_detail("refused", true)
        }
        RequestTokenError::Request(inner) => {
            GhostError::new(ErrorKind::Network, format!("{what} failed: {inner}"))
                .with_detail("server", server_id)
        }
        RequestTokenError::Parse(inner, _) => GhostError::new(
            ErrorKind::Provider,
            format!("{what} answered with something that is not a token response: {inner}"),
        )
        .with_detail("server", server_id),
        RequestTokenError::Other(message) => {
            GhostError::new(ErrorKind::Network, format!("{what} failed: {message}"))
                .with_detail("server", server_id)
        }
    }
}

/// `{origin}/.well-known/{suffix}` and, for a URL with a path, the path-aware
/// variant first: RFC 8414 lets an issuer with a path publish there.
fn well_known(base: &Url, suffix: &str) -> Vec<Url> {
    let mut candidates = Vec::new();
    let path = base.path().trim_end_matches('/');
    if !path.is_empty()
        && let Ok(url) = base.join(&format!("/.well-known/{suffix}{path}"))
    {
        candidates.push(url);
    }
    if let Ok(url) = base.join(&format!("/.well-known/{suffix}")) {
        candidates.push(url);
    }
    candidates
}

impl OAuthFlow {
    /// A flow holding no verifier and no cached discovery.
    pub fn new(options: OAuthFlowOptions) -> OAuthFlow {
        OAuthFlow {
            options,
            verifier: parking_lot::Mutex::new(None),
            endpoints: tokio::sync::Mutex::new(None),
        }
    }

    /// The server this flow is for.
    pub fn server_id(&self) -> &str {
        &self.options.server_id
    }

    /// Where the authorization server sends the operator back.
    pub fn redirect_url(&self) -> &str {
        &self.options.redirect_url
    }

    /// The `state` this attempt routes on.
    pub fn state(&self) -> &str {
        &self.options.state
    }

    /// How this client introduces itself to an authorization server.
    pub fn client_metadata(&self) -> ClientMetadata {
        let scope = self.options.config.scopes.join(" ");
        ClientMetadata {
            client_name: OAUTH_CLIENT_NAME.to_owned(),
            redirect_uris: vec![self.options.redirect_url.clone()],
            grant_types: vec!["authorization_code".to_owned(), "refresh_token".to_owned()],
            response_types: vec!["code".to_owned()],
            token_endpoint_auth_method: "none".to_owned(),
            scope: if scope.is_empty() { None } else { Some(scope) },
        }
    }

    /// The identity the server knows us by.
    ///
    /// Dynamic registration wins over the configured id: if the server issued
    /// us one, that is the identity it knows us by. A `clientId` in
    /// `config.yaml` is the pre-registered case, and the fallback for a server
    /// that does not support registration at all.
    pub fn client_information(&self) -> Option<ClientInformation> {
        let registered: Option<ClientInformation> = read_json(
            self.options
                .store
                .read(&self.options.server_id, McpSecretSlot::Client),
        );
        if registered.is_some() {
            return registered;
        }
        let configured = self.options.config.client_id.trim();
        if configured.is_empty() {
            None
        } else {
            Some(ClientInformation {
                client_id: configured.to_owned(),
                client_secret: None,
            })
        }
    }

    /// Records what registration issued.
    pub fn save_client_information(&self, information: &ClientInformation) -> Result<()> {
        write_json(
            self.options.store.as_ref(),
            &self.options.server_id,
            McpSecretSlot::Client,
            information,
        )
    }

    /// The stored tokens, if any parse.
    pub fn tokens(&self) -> Option<StoredTokens> {
        read_json(
            self.options
                .store
                .read(&self.options.server_id, McpSecretSlot::Tokens),
        )
    }

    /// Records a token response.
    pub fn save_tokens(&self, tokens: &StoredTokens) -> Result<()> {
        write_json(
            self.options.store.as_ref(),
            &self.options.server_id,
            McpSecretSlot::Tokens,
            tokens,
        )
    }

    /// Keeps the verifier for the one exchange it is good for.
    pub fn save_code_verifier(&self, verifier: String) {
        *self.verifier.lock() = Some(verifier);
    }

    /// The outstanding verifier, or `conflict` when none is.
    pub fn code_verifier(&self) -> Result<String> {
        self.verifier.lock().clone().ok_or_else(|| {
            GhostError::new(
                ErrorKind::Conflict,
                format!(
                    "No PKCE verifier is outstanding for MCP server \"{}\"",
                    self.options.server_id
                ),
            )
        })
    }

    /// Hands the operator's link over. Never opens anything.
    pub fn report_authorization_url(&self, url: &Url) {
        (self.options.on_authorization_required)(url.as_str());
    }

    /// The server told us a credential is no longer good.
    ///
    /// Acting on it is what stops an expired refresh token from being retried
    /// forever; without this the operator has to delete the server and add it
    /// back to recover.
    pub fn invalidate_credentials(&self, scope: InvalidationScope) -> Result<()> {
        let store = self.options.store.as_ref();
        let server_id = &self.options.server_id;
        match scope {
            InvalidationScope::Verifier => {
                *self.verifier.lock() = None;
                Ok(())
            }
            InvalidationScope::Discovery => {
                *self.verifier.lock() = None;
                if let Ok(mut cached) = self.endpoints.try_lock() {
                    *cached = None;
                }
                Ok(())
            }
            InvalidationScope::All => {
                *self.verifier.lock() = None;
                store.clear(server_id, None)
            }
            InvalidationScope::Client => store.clear(server_id, Some(McpSecretSlot::Client)),
            InvalidationScope::Tokens => store.clear(server_id, Some(McpSecretSlot::Tokens)),
        }
    }

    /// Whether `tokens` are still good for a call starting now.
    fn is_fresh(&self, tokens: &StoredTokens) -> bool {
        match tokens.expires_at_ms {
            None => true,
            Some(at) => self.options.clock.now_ms() + EXPIRY_SKEW_MS < at,
        }
    }

    /// The `Authorization` value for a request to `resource_url`, refreshing
    /// first when the stored access token has expired.
    ///
    /// `None` means the server has not been authorized yet, or its refresh
    /// token was refused: either way the connect proceeds unauthenticated and
    /// the server's 401 is what starts the flow.
    pub async fn access_token(&self, resource_url: &str) -> Result<Option<String>> {
        let Some(tokens) = self.tokens() else {
            return Ok(None);
        };
        if self.is_fresh(&tokens) {
            return Ok(Some(tokens.access_token));
        }
        let Some(refresh) = tokens.refresh_token.clone() else {
            self.invalidate_credentials(InvalidationScope::Tokens)?;
            return Ok(None);
        };
        match self.refresh(resource_url, &refresh).await {
            Ok(fresh) => Ok(Some(fresh.access_token)),
            Err(error) if error.details.get("refused") == Some(&serde_json::Value::Bool(true)) => {
                // A refused refresh is the server saying the grant is over.
                // Keeping the tokens would retry the same answer forever.
                tracing::debug!(
                    server = %self.options.server_id,
                    error = %error.message,
                    "mcp oauth refresh refused; re-authorization needed"
                );
                self.invalidate_credentials(InvalidationScope::Tokens)?;
                Ok(None)
            }
            Err(error) => Err(error),
        }
    }

    /// Where the operator has to go. Runs discovery and registration as
    /// needed, mints the PKCE pair, reports the URL, and returns it.
    pub async fn begin_authorization(&self, resource_url: &str) -> Result<Url> {
        let endpoints = self.endpoints(resource_url).await?;
        let client = self.client(&endpoints).await?;

        let mut bytes = vec![0u8; VERIFIER_BYTES];
        self.options.random.fill(&mut bytes);
        let verifier = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(&bytes);
        let challenge =
            PkceCodeChallenge::from_code_verifier_sha256(&PkceCodeVerifier::new(verifier.clone()));
        self.save_code_verifier(verifier);

        let state = self.options.state.clone();
        let (url, _) = client
            .authorize_url(move || CsrfToken::new(state))
            .add_scopes(self.options.config.scopes.iter().cloned().map(Scope::new))
            .set_pkce_challenge(challenge)
            // RFC 8707: the token is for this server and no other.
            .add_extra_param("resource", resource_url.to_owned())
            .url();

        self.report_authorization_url(&url);
        Ok(url)
    }

    /// Exchanges the code the callback received and stores what came back.
    pub async fn finish_authorization(&self, resource_url: &str, code: &str) -> Result<()> {
        let verifier = self.code_verifier()?;
        let endpoints = self.endpoints(resource_url).await?;
        let client = self.client(&endpoints).await?;
        let http = self.http();
        let response = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier))
            .add_extra_param("resource", resource_url.to_owned())
            .request_async(&http)
            .await
            .map_err(|error| {
                token_error(
                    &self.options.server_id,
                    "The authorization code exchange",
                    error,
                )
            })?;
        *self.verifier.lock() = None;
        self.store_response(&response, None).map(|_| ())
    }

    /// The refresh grant.
    async fn refresh(&self, resource_url: &str, refresh_token: &str) -> Result<StoredTokens> {
        let endpoints = self.endpoints(resource_url).await?;
        let client = self.client(&endpoints).await?;
        let http = self.http();
        let response = client
            .exchange_refresh_token(&RefreshToken::new(refresh_token.to_owned()))
            .add_extra_param("resource", resource_url.to_owned())
            .request_async(&http)
            .await
            .map_err(|error| token_error(&self.options.server_id, "The token refresh", error))?;
        // A server may rotate the refresh token or leave it out of the
        // response; either way the one that worked is the one to keep.
        self.store_response(&response, Some(refresh_token))
    }

    fn store_response(
        &self,
        response: &oauth2::basic::BasicTokenResponse,
        previous_refresh: Option<&str>,
    ) -> Result<StoredTokens> {
        let expires_at_ms = response.expires_in().and_then(|expires_in| {
            i64::try_from(expires_in.as_millis())
                .ok()
                .map(|ms| self.options.clock.now_ms() + ms)
        });
        let tokens = StoredTokens {
            access_token: response.access_token().secret().clone(),
            token_type: format!("{:?}", response.token_type()).to_lowercase(),
            refresh_token: response
                .refresh_token()
                .map(|token| token.secret().clone())
                .or_else(|| previous_refresh.map(str::to_owned)),
            expires_at_ms,
            scope: response.scopes().map(|scopes| {
                scopes
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            }),
        };
        self.save_tokens(&tokens)?;
        Ok(tokens)
    }

    fn http(&self) -> HttpAdapter {
        HttpAdapter(self.options.http.clone())
    }

    /// The configured `oauth2` client, registering first if this server has
    /// never issued us an id.
    async fn client(&self, endpoints: &Endpoints) -> Result<ConfiguredClient> {
        let information = match self.client_information() {
            Some(information) => information,
            None => self.register(endpoints).await?,
        };
        let auth = AuthUrl::from_url(endpoints.authorization.clone());
        let token = TokenUrl::from_url(endpoints.token.clone());
        let redirect = RedirectUrl::new(self.options.redirect_url.clone()).map_err(|error| {
            GhostError::new(ErrorKind::Internal, "The callback URL is not a URL").with_source(error)
        })?;
        let mut client = BasicClient::new(ClientId::new(information.client_id))
            .set_auth_uri(auth)
            .set_token_uri(token)
            .set_redirect_uri(redirect)
            .set_auth_type(AuthType::RequestBody);
        if let Some(secret) = information.client_secret {
            client = client.set_client_secret(ClientSecret::new(secret));
        }
        Ok(client)
    }

    /// RFC 7591 dynamic client registration.
    async fn register(&self, endpoints: &Endpoints) -> Result<ClientInformation> {
        let Some(registration) = &endpoints.registration else {
            return Err(GhostError::new(
                ErrorKind::Config,
                format!(
                    "MCP server \"{}\" needs OAuth but names no clientId and its authorization server offers no registration",
                    self.options.server_id
                ),
            )
            .with_detail("server", self.options.server_id.as_str()));
        };
        let response = self
            .options
            .http
            .post(registration.clone())
            .json(&self.client_metadata())
            .send()
            .await
            .map_err(|error| {
                GhostError::new(
                    ErrorKind::Network,
                    format!("OAuth client registration failed: {error}"),
                )
                .with_detail("server", self.options.server_id.as_str())
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(oauth_error(
                &self.options.server_id,
                format!("OAuth client registration was refused with HTTP {status}"),
            ));
        }
        let issued: RegistrationResponse = response.json().await.map_err(|error| {
            GhostError::new(
                ErrorKind::Provider,
                format!("OAuth client registration answered with something unreadable: {error}"),
            )
            .with_detail("server", self.options.server_id.as_str())
        })?;
        let information = ClientInformation {
            client_id: issued.client_id,
            client_secret: issued.client_secret,
        };
        self.save_client_information(&information)?;
        Ok(information)
    }

    /// The endpoints for `resource_url`, discovered once per flow.
    async fn endpoints(&self, resource_url: &str) -> Result<Endpoints> {
        let mut cached = self.endpoints.lock().await;
        if let Some(endpoints) = cached.as_ref() {
            return Ok(endpoints.clone());
        }
        let endpoints = match self.discover(resource_url).await {
            Some(discovered) => {
                self.guard(&discovered).await?;
                discovered
            }
            None => self.configured()?,
        };
        *cached = Some(endpoints.clone());
        Ok(endpoints)
    }

    /// The operator-typed endpoints, the fallback when discovery finds nothing.
    fn configured(&self) -> Result<Endpoints> {
        let parse = |what: &str, raw: &str| {
            Url::parse(raw).map_err(|_| {
                GhostError::new(
                    ErrorKind::Config,
                    format!(
                        "MCP server \"{}\": oauth.{what} \"{raw}\" is not a URL",
                        self.options.server_id
                    ),
                )
                .with_detail("server", self.options.server_id.as_str())
            })
        };
        Ok(Endpoints {
            authorization: parse("authUrl", &self.options.config.auth_url)?,
            token: parse("tokenUrl", &self.options.config.token_url)?,
            registration: None,
            discovered: false,
        })
    }

    /// RFC 9728 then RFC 8414. `None` when the server publishes neither; a
    /// metadata document that exists but is unreadable also lands here, so a
    /// half-configured server falls back to the config rather than failing.
    async fn discover(&self, resource_url: &str) -> Option<Endpoints> {
        let resource = Url::parse(resource_url).ok()?;
        let issuer = self
            .protected_resource_issuer(&resource)
            .await
            .unwrap_or(resource);
        for candidate in well_known(&issuer, "oauth-authorization-server") {
            let Some(metadata) = self.fetch_json::<ServerMetadata>(candidate).await else {
                continue;
            };
            let authorization = Url::parse(&metadata.authorization).ok()?;
            let token = Url::parse(&metadata.token).ok()?;
            let registration = metadata
                .registration
                .as_deref()
                .and_then(|raw| Url::parse(raw).ok());
            return Some(Endpoints {
                authorization,
                token,
                registration,
                discovered: true,
            });
        }
        None
    }

    async fn protected_resource_issuer(&self, resource: &Url) -> Option<Url> {
        for candidate in well_known(resource, "oauth-protected-resource") {
            if let Some(metadata) = self.fetch_json::<ResourceMetadata>(candidate).await {
                return metadata
                    .authorization_servers
                    .first()
                    .and_then(|issuer| Url::parse(issuer).ok());
            }
        }
        None
    }

    async fn fetch_json<T: for<'de> Deserialize<'de>>(&self, url: Url) -> Option<T> {
        let response = self.options.http.get(url).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        response.json::<T>().await.ok()
    }

    /// Discovered endpoints must pass the network policy; see the module docs.
    async fn guard(&self, endpoints: &Endpoints) -> Result<()> {
        let Some(guard) = &self.options.guard else {
            return Ok(());
        };
        let mut targets = vec![&endpoints.authorization, &endpoints.token];
        targets.extend(endpoints.registration.as_ref());
        for target in targets {
            validate_target(target.as_str(), &guard.policy, guard.resolver.as_ref())
                .await
                .map_err(|error| {
                    GhostError::new(
                        ErrorKind::Network,
                        format!(
                            "MCP server \"{}\" named an OAuth endpoint this client will not use: {}",
                            self.options.server_id, error.message
                        ),
                    )
                    .with_detail("server", self.options.server_id.as_str())
                    .with_detail("endpoint", target.as_str())
                })?;
        }
        Ok(())
    }
}
