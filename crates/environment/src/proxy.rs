//! Destination-restricted HTTP proxy. TLS tunnels are not decrypted.
use darkwire_core::Result;
use darkwire_security::environment::invalid;
use darkwire_security::{
    AllowList, EgressAllow, HickoryResolver, NetworkPolicy, PinnedTarget, validate_target,
};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    time::timeout,
};

/// What a validated request turned into.
enum Opened {
    /// A `CONNECT` tunnel, relayed both ways.
    Tunnel(TcpStream),
    /// One forwarded request, whose answer is copied back.
    Forwarded(TcpStream),
}

async fn connection(
    mut client: TcpStream,
    allow: &AllowList,
    resolver: &HickoryResolver,
) -> Result<()> {
    let opened = timeout(IDLE_TIMEOUT, open(&mut client, allow, resolver))
        .await
        .map_err(|_| invalid("Proxy request was not completed in time"))??;
    let relayed = match opened {
        Opened::Tunnel(upstream) => relay(client, upstream).await,
        Opened::Forwarded(mut upstream) => copy_idle(&mut upstream, &mut client).await,
    };
    relayed.map_err(|e| invalid(e.to_string()))
}

/// Reads and validates the request, and dials the one address it may reach.
async fn open(
    client: &mut TcpStream,
    allow: &AllowList,
    resolver: &HickoryResolver,
) -> Result<Opened> {
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        if header.len() >= 16384 {
            return Err(invalid("Proxy headers exceed limit"));
        }
        header.push(client.read_u8().await.map_err(|e| invalid(e.to_string()))?);
    }
    let header = std::str::from_utf8(&header).map_err(|_| invalid("Invalid proxy headers"))?;
    let mut lines = header.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split_whitespace();
    let method = request
        .next()
        .ok_or_else(|| invalid("Missing HTTP method"))?;
    let destination = request
        .next()
        .ok_or_else(|| invalid("Missing proxy destination"))?;
    if request.next() != Some("HTTP/1.1") || request.next().is_some() {
        return Err(invalid("Expected HTTP/1.1"));
    }
    let tunnel = method == "CONNECT";
    let url = if tunnel {
        format!("https://{destination}/")
    } else {
        destination.to_owned()
    };
    // Check the allowlist before any DNS query: rejected hostnames must not
    // themselves become an exfiltration channel through the resolver.
    let parsed = approved_destination(&url, allow, tunnel)?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("Missing destination port"))?;
    // The same entries the allow-list check above used. The default policy
    // refuses every private range, so a gateway whose list names 10.0.0.0/8
    // would approve the destination here and refuse it a line later.
    let policy = NetworkPolicy {
        allow: EgressAllow::Only(allow.clone()),
        ..NetworkPolicy::default()
    };
    let target = validate_target(parsed.as_str(), &policy, resolver).await?;
    let (forwarded, body_length) = if tunnel {
        (String::new(), 0)
    } else {
        forwarded_request(method, lines, &target)?
    };
    // Dial only the validated address, never resolve the hostname a second time.
    let address = target
        .addresses
        .first()
        .ok_or_else(|| invalid("No validated destination"))?;
    let mut upstream = TcpStream::connect(SocketAddr::new(*address, port))
        .await
        .map_err(|e| invalid(e.to_string()))?;
    if tunnel {
        client
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .map_err(|e| invalid(e.to_string()))?;
        return Ok(Opened::Tunnel(upstream));
    }
    let mut body = vec![0; body_length];
    client
        .read_exact(&mut body)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    upstream
        .write_all(forwarded.as_bytes())
        .await
        .map_err(|e| invalid(e.to_string()))?;
    upstream
        .write_all(&body)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    // Forward exactly one validated request. Never tunnel pipelined HTTP
    // requests that could change Host on a shared destination IP.
    Ok(Opened::Forwarded(upstream))
}

fn idle() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "Proxy connection went idle")
}

/// Copies both ways until both sides close, or until neither has moved for
/// [`IDLE_TIMEOUT`]. One clock for both directions, so a long download with a
/// quiet client stays open.
async fn relay<A, B>(a: A, b: B) -> std::io::Result<()>
where
    A: AsyncRead + AsyncWrite + Unpin,
    B: AsyncRead + AsyncWrite + Unpin,
{
    let (mut a_read, mut a_write) = tokio::io::split(a);
    let (mut b_read, mut b_write) = tokio::io::split(b);
    let mut up = vec![0; 16 * 1024];
    let mut down = vec![0; 16 * 1024];
    let (mut a_open, mut b_open) = (true, true);
    while a_open || b_open {
        tokio::select! {
            read = a_read.read(&mut up), if a_open => match read? {
                0 => {
                    a_open = false;
                    b_write.shutdown().await?;
                }
                n => timeout(IDLE_TIMEOUT, b_write.write_all(&up[..n])).await.map_err(|_| idle())??,
            },
            read = b_read.read(&mut down), if b_open => match read? {
                0 => {
                    b_open = false;
                    a_write.shutdown().await?;
                }
                n => timeout(IDLE_TIMEOUT, a_write.write_all(&down[..n])).await.map_err(|_| idle())??,
            },
            () = tokio::time::sleep(IDLE_TIMEOUT) => return Err(idle()),
        }
    }
    Ok(())
}

/// Copies one way until the reader closes or goes quiet for [`IDLE_TIMEOUT`].
async fn copy_idle<R, W>(reader: &mut R, writer: &mut W) -> std::io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0; 16 * 1024];
    loop {
        let n = timeout(IDLE_TIMEOUT, reader.read(&mut buffer))
            .await
            .map_err(|_| idle())??;
        if n == 0 {
            return Ok(());
        }
        timeout(IDLE_TIMEOUT, writer.write_all(&buffer[..n]))
            .await
            .map_err(|_| idle())??;
    }
}

/// The request line and headers to forward upstream, plus the body length the
/// caller must then read from the client.
///
/// Every header is inspected rather than relayed: an allow-listed destination
/// is only a boundary while the request that reaches it is the one that was
/// checked. A second `Host`, a second `Content-Length`, or any framing that
/// lets one connection carry two requests, would let the second request name a
/// destination nothing approved.
fn forwarded_request<'a>(
    method: &str,
    lines: impl Iterator<Item = &'a str>,
    target: &PinnedTarget,
) -> Result<(String, usize)> {
    let path = format!(
        "{}{}",
        target.url.path(),
        target
            .url
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default()
    );
    let mut forwarded = format!("{method} {path} HTTP/1.1\r\n");
    let mut body_length = 0usize;
    let mut host_count = 0;
    let mut length_count = 0;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| invalid("Invalid HTTP header"))?;
        if name.starts_with(' ')
            || name.starts_with('\t')
            || name.eq_ignore_ascii_case("transfer-encoding")
            || name.eq_ignore_ascii_case("upgrade")
            || name.eq_ignore_ascii_case("expect")
        {
            return Err(invalid("Unsupported HTTP framing"));
        }
        if name.eq_ignore_ascii_case("content-length") {
            length_count += 1;
            body_length = value
                .trim()
                .parse()
                .map_err(|_| invalid("Invalid Content-Length"))?;
            if length_count != 1 || body_length > 1024 * 1024 {
                return Err(invalid("Invalid or oversized HTTP body"));
            }
        }
        if name.eq_ignore_ascii_case("host") {
            host_count += 1;
            if !value.trim().eq_ignore_ascii_case(&target.host)
                && !value
                    .trim()
                    .eq_ignore_ascii_case(&format!("{}:80", target.host))
            {
                return Err(invalid("Host header does not match approved destination"));
            }
        }
        if name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        forwarded.push_str(line);
        forwarded.push_str("\r\n");
    }
    if host_count != 1 {
        return Err(invalid("Exactly one matching Host header is required"));
    }
    forwarded.push_str("Connection: close\r\n\r\n");
    Ok((forwarded, body_length))
}

fn approved_destination(url: &str, allow: &AllowList, tunnel: bool) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("Invalid proxy destination"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| invalid("Missing destination host"))?;
    let port = parsed.port_or_known_default();
    // `match_host` sends an address destination to the blocks and a name to the
    // names, so the one entry list serves both without either authorising the
    // other. See the grammar's module header.
    if allow.match_host(host).is_none()
        || (tunnel && (parsed.scheme() != "https" || port != Some(443)))
        || (!tunnel && (parsed.scheme() != "http" || port != Some(80)))
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.fragment().is_some()
    {
        return Err(invalid("Proxy destination is not approved"));
    }
    Ok(parsed)
}

/// How long a proxied connection may go without a byte in either direction.
///
/// A bound rather than a courtesy: the gateway holds a fixed number of slots,
/// and a connection nobody uses is a slot no later command can have. Idle
/// rather than total, so a long download is not cut off part way.
const IDLE_TIMEOUT: Duration = Duration::from_mins(5);

/// Run inside the gateway as UID 65532, after firewall installation.
pub async fn serve(entries: Vec<String>) -> Result<()> {
    // Parsed once, at startup, so a malformed entry stops the gateway rather
    // than failing every request with a message nobody reads.
    let allow = Arc::new(AllowList::parse(&entries)?);
    // One resolver, so its cache serves every connection.
    let resolver = Arc::new(HickoryResolver::new()?);
    let listener = TcpListener::bind("127.0.0.1:3128")
        .await
        .map_err(|e| invalid(e.to_string()))?;
    std::fs::write("/tmp/ready", b"ready").map_err(|e| invalid(e.to_string()))?;
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let (client, _) = listener
            .accept()
            .await
            .map_err(|e| invalid(e.to_string()))?;
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            continue;
        };
        let allow = Arc::clone(&allow);
        let resolver = Arc::clone(&resolver);
        tokio::spawn(async move {
            let _slot = slot;
            let _ = connection(client, &allow, &resolver).await;
        });
    }
}

/// Inline because `approved_destination`, `forwarded_request`, `relay` and
/// `copy_idle` are private and have no public path: each is only reachable
/// through a live socket, and asserting them through one would test the
/// network rather than the rule.
#[cfg(test)]
mod tests {
    use super::{IDLE_TIMEOUT, approved_destination, copy_idle, forwarded_request, relay};
    use darkwire_security::{AllowList, PinnedTarget};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt, duplex};

    #[tokio::test(start_paused = true)]
    async fn a_relay_that_keeps_moving_outlives_the_idle_limit() {
        let (client, mut client_far) = duplex(64);
        let (upstream, mut upstream_far) = duplex(64);
        let relayed = tokio::spawn(relay(client, upstream));
        for _ in 0..3 {
            tokio::time::sleep(Duration::from_mins(4)).await;
            upstream_far.write_all(b"x").await.expect("written");
            let mut byte = [0; 1];
            client_far.read_exact(&mut byte).await.expect("relayed");
        }
        assert!(!relayed.is_finished());

        tokio::time::sleep(IDLE_TIMEOUT + Duration::from_secs(1)).await;
        let error = relayed.await.expect("joined").expect_err("idle");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test(start_paused = true)]
    async fn a_relay_ends_when_both_sides_close() {
        let (client, client_far) = duplex(64);
        let (upstream, upstream_far) = duplex(64);
        drop((client_far, upstream_far));
        assert!(relay(client, upstream).await.is_ok());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_answer_is_given_up_on() {
        let (mut upstream, _upstream_far) = duplex(64);
        let (mut client, _client_far) = duplex(64);
        let error = copy_idle(&mut upstream, &mut client)
            .await
            .expect_err("idle");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    fn allow(entries: &[&str]) -> AllowList {
        AllowList::parse(
            &entries
                .iter()
                .map(|entry| (*entry).to_owned())
                .collect::<Vec<_>>(),
        )
        .expect("the entries parse")
    }

    /// A destination already validated, as `forwarded_request` receives one.
    fn target(url: &str) -> PinnedTarget {
        let url: reqwest::Url = url.parse().expect("a URL");
        let host = url.host_str().expect("a host").to_owned();
        PinnedTarget {
            url,
            host,
            addresses: vec![std::net::IpAddr::from([93, 184, 216, 34])],
        }
    }

    /// The header block as it arrives, minus the request line the caller has
    /// already consumed.
    fn forward(headers: &[&str], url: &str) -> darkwire_core::Result<(String, usize)> {
        forwarded_request("GET", headers.iter().copied(), &target(url))
    }

    #[test]
    fn forwards_exactly_one_request_to_the_approved_host() {
        let (forwarded, body) = forward(
            &["Host: example.com", "Accept: */*", ""],
            "http://example.com/a?b=1",
        )
        .expect("an approved request");
        // The path comes from the validated URL, not from what the client
        // wrote: the destination that was checked is the one that is reached.
        assert!(
            forwarded.starts_with("GET /a?b=1 HTTP/1.1\r\n"),
            "{forwarded}"
        );
        assert!(forwarded.contains("Accept: */*\r\n"));
        // Closed after one request, so a second cannot ride the same socket to
        // a destination nothing approved.
        assert!(
            forwarded.ends_with("Connection: close\r\n\r\n"),
            "{forwarded}"
        );
        assert_eq!(body, 0);
    }

    #[test]
    fn strips_the_hop_by_hop_headers_rather_than_relaying_them() {
        let (forwarded, _) = forward(
            &[
                "Host: example.com",
                "Proxy-Authorization: Basic abc",
                "Proxy-Connection: keep-alive",
                "Connection: keep-alive",
                "",
            ],
            "http://example.com/",
        )
        .expect("an approved request");
        for stripped in ["Proxy-Authorization", "Proxy-Connection", "keep-alive"] {
            assert!(!forwarded.contains(stripped), "{stripped}: {forwarded}");
        }
    }

    #[test]
    fn refuses_any_framing_that_could_carry_a_second_request() {
        // Each of these lets one connection be read as two requests, and the
        // second would name a destination nothing checked.
        for headers in [
            vec!["Host: example.com", "Transfer-Encoding: chunked", ""],
            vec!["Host: example.com", "Upgrade: websocket", ""],
            vec!["Host: example.com", "Expect: 100-continue", ""],
            vec![
                "Host: example.com",
                "Content-Length: 1",
                "Content-Length: 2",
                "",
            ],
            vec![" Host: example.com", ""],
        ] {
            assert!(
                forward(&headers, "http://example.com/").is_err(),
                "{headers:?}"
            );
        }
    }

    #[test]
    fn requires_exactly_one_host_header_naming_the_approved_destination() {
        for headers in [
            vec!["Accept: */*", ""],
            vec!["Host: example.com", "Host: example.com", ""],
            vec!["Host: evil.example", ""],
            vec!["Host: example.com:8080", ""],
        ] {
            assert!(
                forward(&headers, "http://example.com/").is_err(),
                "{headers:?}"
            );
        }
        // The default port spelled out is the same destination.
        assert!(forward(&["Host: example.com:80", ""], "http://example.com/").is_ok());
    }

    #[test]
    fn refuses_a_body_that_is_unparseable_or_larger_than_the_bound() {
        assert!(
            forward(
                &["Host: example.com", "Content-Length: two", ""],
                "http://example.com/"
            )
            .is_err()
        );
        assert!(
            forward(
                &["Host: example.com", "Content-Length: 1048577", ""],
                "http://example.com/"
            )
            .is_err()
        );
        let (_, body) = forward(
            &["Host: example.com", "Content-Length: 12", ""],
            "http://example.com/",
        )
        .expect("a bounded body");
        assert_eq!(body, 12);
    }

    #[test]
    fn refuses_a_header_line_that_is_not_a_header() {
        assert!(
            forward(
                &["Host: example.com", "not-a-header", ""],
                "http://example.com/"
            )
            .is_err()
        );
    }

    #[test]
    fn destination_is_checked_without_dns() {
        let allowed = allow(&["example.com"]);
        assert!(approved_destination("http://example.com/a", &allowed, false).is_ok());
        assert!(approved_destination("https://example.com/", &allowed, true).is_ok());
        for url in [
            "http://exfil.example.com/",
            "http://example.com.evil/",
            "http://example.com@evil/",
            "http://u@example.com/",
            "http://example.com:8080/",
            "http://127.0.0.1/",
            "https://example.com/",
        ] {
            assert!(approved_destination(url, &allowed, false).is_err(), "{url}");
        }
    }

    /// The one entry list serves both shapes of destination, and neither
    /// authorises the other: a block never admits a name, a name never admits
    /// an address.
    #[test]
    fn an_address_destination_matches_a_block_and_a_name_does_not() {
        let blocks = allow(&["10.0.0.0/8"]);
        assert!(approved_destination("http://10.0.0.5/", &blocks, false).is_ok());
        assert!(approved_destination("http://internal.corp/", &blocks, false).is_err());

        let names = allow(&["internal.corp"]);
        assert!(approved_destination("http://internal.corp/", &names, false).is_ok());
        assert!(approved_destination("http://10.0.0.5/", &names, false).is_err());
    }

    #[test]
    fn a_suffix_entry_admits_a_subdomain() {
        let allowed = allow(&[".example.com"]);
        for url in [
            "http://example.com/",
            "http://api.example.com/",
            "http://a.b.example.com/",
        ] {
            assert!(approved_destination(url, &allowed, false).is_ok(), "{url}");
        }
        assert!(approved_destination("http://notexample.com/", &allowed, false).is_err());
    }

    /// Scheme and port stay pinned whatever the entry looks like, so a name is
    /// reachable over 80 and 443 and nowhere else.
    #[test]
    fn the_port_pin_survives_every_entry_shape() {
        for entries in [vec!["example.com"], vec![".example.com"]] {
            let allowed = allow(&entries);
            assert!(approved_destination("http://example.com:8080/", &allowed, false).is_err());
            assert!(approved_destination("https://example.com:8443/", &allowed, true).is_err());
        }
        let blocks = allow(&["10.0.0.0/8"]);
        assert!(approved_destination("http://10.0.0.5:8080/", &blocks, false).is_err());
    }
}
