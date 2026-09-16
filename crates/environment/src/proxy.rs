//! Destination-restricted HTTP proxy. TLS tunnels are not decrypted.
use darkwire_core::Result;
use darkwire_security::environment::invalid;
use darkwire_security::{HickoryResolver, NetworkPolicy, PinnedTarget, validate_target};
use std::{net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

async fn connection(mut client: TcpStream, hosts: &[String]) -> Result<()> {
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
    let parsed = approved_destination(&url, hosts, tunnel)?;
    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| invalid("Missing destination port"))?;
    let resolver = HickoryResolver::new()?;
    let target = validate_target(parsed.as_str(), &NetworkPolicy::default(), &resolver).await?;
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
    } else {
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
        tokio::io::copy(&mut upstream, &mut client)
            .await
            .map_err(|e| invalid(e.to_string()))?;
        return Ok(());
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream)
        .await
        .map_err(|e| invalid(e.to_string()))?;
    Ok(())
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

fn approved_destination(url: &str, hosts: &[String], tunnel: bool) -> Result<reqwest::Url> {
    let parsed = reqwest::Url::parse(url).map_err(|_| invalid("Invalid proxy destination"))?;
    let host = parsed
        .host_str()
        .ok_or_else(|| invalid("Missing destination host"))?;
    let port = parsed.port_or_known_default();
    if !hosts.iter().any(|h| h.eq_ignore_ascii_case(host))
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

/// How long one proxied connection may stay open.
///
/// A bound rather than a courtesy: the gateway holds a fixed number of slots,
/// and a connection nobody closes is a slot no later command can have.
const PROXY_TIMEOUT: Duration = Duration::from_mins(5);

/// Run inside the gateway as UID 65532, after firewall installation.
pub async fn serve(hosts: Vec<String>) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:3128")
        .await
        .map_err(|e| invalid(e.to_string()))?;
    std::fs::write("/tmp/ready", b"ready").map_err(|e| invalid(e.to_string()))?;
    let hosts = Arc::new(hosts);
    let slots = Arc::new(tokio::sync::Semaphore::new(64));
    loop {
        let (client, _) = listener
            .accept()
            .await
            .map_err(|e| invalid(e.to_string()))?;
        let Ok(slot) = Arc::clone(&slots).try_acquire_owned() else {
            continue;
        };
        let hosts = Arc::clone(&hosts);
        tokio::spawn(async move {
            let _slot = slot;
            let _ = tokio::time::timeout(PROXY_TIMEOUT, connection(client, &hosts)).await;
        });
    }
}

/// Inline because `approved_destination` and `forwarded_request` are private
/// and have no public path: both are halves of one decision that is only
/// reachable through a live socket, and asserting them through one would test
/// the network rather than the rule.
#[cfg(test)]
mod tests {
    use super::{approved_destination, forwarded_request};
    use darkwire_security::PinnedTarget;

    /// A destination already validated, as `forwarded_request` receives one.
    fn target(url: &str) -> PinnedTarget {
        let url: reqwest::Url = url.parse().expect("a URL");
        let host = url.host_str().expect("a host").to_owned();
        PinnedTarget {
            url,
            host,
            addresses: vec![std::net::IpAddr::from([93, 184, 216, 34])],
            exempt: false,
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
        let hosts = vec!["example.com".into()];
        assert!(approved_destination("http://example.com/a", &hosts, false).is_ok());
        assert!(approved_destination("https://example.com/", &hosts, true).is_ok());
        for url in [
            "http://exfil.example.com/",
            "http://example.com.evil/",
            "http://example.com@evil/",
            "http://u@example.com/",
            "http://example.com:8080/",
            "http://127.0.0.1/",
            "https://example.com/",
        ] {
            assert!(approved_destination(url, &hosts, false).is_err(), "{url}");
        }
    }
}
