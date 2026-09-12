//! Test doubles for the injected seams, behind the `testkit` feature.

use std::collections::HashMap;
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

use ghostai_core::{ErrorKind, GhostError, Result};

use crate::fetch::DnsResolver;
use crate::random::RandomSource;

/// A source that repeats a byte pattern, so a test can pin an IV or a nonce.
///
/// Never usable in production by construction: it lives behind a cargo feature
/// no binary turns on.
#[derive(Debug, Clone)]
pub struct FixedRandom {
    pattern: Vec<u8>,
}

impl FixedRandom {
    /// Fills every buffer with `byte`.
    pub fn constant(byte: u8) -> FixedRandom {
        FixedRandom {
            pattern: vec![byte],
        }
    }

    /// Fills buffers by cycling through `pattern`. An empty pattern fills zeros.
    pub fn pattern(pattern: &[u8]) -> FixedRandom {
        FixedRandom {
            pattern: pattern.to_vec(),
        }
    }
}

impl RandomSource for FixedRandom {
    fn fill(&self, buf: &mut [u8]) {
        for (index, slot) in buf.iter_mut().enumerate() {
            *slot = self
                .pattern
                .get(index % self.pattern.len().max(1))
                .copied()
                .unwrap_or(0);
        }
    }
}

/// A resolver that answers from a table and never touches DNS.
#[derive(Debug, Clone, Default)]
pub struct StaticResolver {
    answers: HashMap<String, Vec<IpAddr>>,
}

impl StaticResolver {
    /// A resolver with no answers; every lookup fails.
    pub fn new() -> StaticResolver {
        StaticResolver::default()
    }

    /// Adds the addresses `host` resolves to.
    #[must_use]
    pub fn with(mut self, host: &str, addresses: &[IpAddr]) -> StaticResolver {
        self.answers.insert(host.to_owned(), addresses.to_vec());
        self
    }
}

impl DnsResolver for StaticResolver {
    fn resolve<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IpAddr>>> + Send + 'a>> {
        let answer = self.answers.get(host).cloned();
        Box::pin(async move {
            answer.ok_or_else(|| {
                GhostError::new(ErrorKind::Network, format!("No static answer for {host}"))
            })
        })
    }
}
