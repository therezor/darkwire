//! The manifest is the only path to a served route, so it is the thing worth
//! asserting about directly.
//!
//! `packages/protocol/schema/routes.json` does not exist in this repository, so
//! there is no generated listing to diff against. What is checked instead are
//! the properties that listing would have caught: the count, that no id or
//! method-and-path pair repeats, and that the auth classes are exactly the ones
//! the security argument depends on — four public routes and one signed one,
//! named.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

use ghostai_server::manifest::{ROUTE_MANIFEST, RouteAuth, RouteId, RouteMethod};

/// The routes every build serves. The `test-hooks` entries are extra, and the
/// feature is on for this test binary, so the base set is what is filtered for.
const BASE_ROUTES: usize = 63;

fn base() -> Vec<&'static ghostai_server::manifest::Route> {
    ROUTE_MANIFEST
        .iter()
        .filter(|route| !route.path.starts_with("/api/_test/"))
        .collect()
}

#[test]
fn the_manifest_carries_sixty_three_routes() {
    assert_eq!(base().len(), BASE_ROUTES);
}

#[test]
fn no_route_id_appears_twice() {
    let mut seen: Vec<RouteId> = Vec::new();
    for route in ROUTE_MANIFEST {
        assert!(
            !seen.contains(&route.id),
            "duplicate route id: {}",
            route.id.as_str()
        );
        seen.push(route.id);
    }
}

#[test]
fn no_method_and_path_pair_appears_twice() {
    let mut seen: Vec<(RouteMethod, &str)> = Vec::new();
    for route in ROUTE_MANIFEST {
        let pair = (route.method, route.path);
        assert!(
            !seen.contains(&pair),
            "duplicate route: {} {}",
            route.method.as_str(),
            route.path
        );
        seen.push(pair);
    }
}

#[test]
fn exactly_four_routes_are_public_and_they_are_the_four_that_have_to_be() {
    let public: Vec<&str> = base()
        .iter()
        .filter(|route| route.auth == RouteAuth::Public)
        .map(|route| route.id.as_str())
        .collect();
    // The liveness probe, the login that mints the credential, and the two
    // first-run setup routes that stop existing once a password is set.
    assert_eq!(
        public,
        ["system.health", "auth.login", "setup.status", "setup.claim"]
    );
}

#[test]
fn exactly_one_route_is_signed_and_it_is_the_media_route() {
    let signed: Vec<&str> = ROUTE_MANIFEST
        .iter()
        .filter(|route| route.auth == RouteAuth::Signed)
        .map(|route| route.id.as_str())
        .collect();
    // `<img src>` can carry neither a header nor, reliably, a
    // `SameSite=Strict` cookie, which is the whole reason this carrier exists.
    assert_eq!(signed, ["media.get"]);
}

#[test]
fn every_other_route_requires_a_session() {
    for route in base() {
        if route.auth == RouteAuth::Public || route.auth == RouteAuth::Signed {
            continue;
        }
        assert_eq!(
            route.auth,
            RouteAuth::Required,
            "{} is neither public, signed nor required",
            route.id.as_str()
        );
    }
}

#[test]
fn the_socket_requires_a_session() {
    let socket = ROUTE_MANIFEST
        .iter()
        .find(|route| route.id == RouteId::WsConnect)
        .expect("the manifest carries the socket");
    // An unauthenticated upgrade is a shell-capable agent that anyone who can
    // reach the port may drive, which is the exact failure the boot policy
    // refuses to start for.
    assert_eq!(socket.auth, RouteAuth::Required);
    assert_eq!(socket.path, "/ws");
    assert_eq!(socket.method, RouteMethod::GET);
}

#[test]
fn every_api_route_lives_under_the_api_prefix() {
    for route in base() {
        if route.id == RouteId::WsConnect {
            continue;
        }
        assert!(
            route.path.starts_with("/api/"),
            "{} is outside /api",
            route.path
        );
    }
}

#[test]
fn a_colon_parameter_becomes_an_axum_brace_parameter() {
    let media = ROUTE_MANIFEST
        .iter()
        .find(|route| route.id == RouteId::MediaGet)
        .expect("the manifest carries the media route");
    // The manifest keeps the colon form because that is what the OpenAPI
    // document and the API reference say; axum refuses it outright, so the
    // rewrite happens once, on the way into the router.
    assert_eq!(media.path, "/api/media/:token");
    assert_eq!(media.axum_path(), "/api/media/{token}");
}

#[test]
fn a_path_with_no_parameter_is_unchanged() {
    for route in ROUTE_MANIFEST {
        if !route.path.contains(':') {
            assert_eq!(route.axum_path(), route.path);
        }
    }
}

#[test]
fn every_id_has_a_distinct_dotted_name() {
    let mut names: Vec<&str> = ROUTE_MANIFEST
        .iter()
        .map(|route| route.id.as_str())
        .collect();
    let count = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), count);
}

#[cfg(feature = "test-hooks")]
#[test]
fn the_test_hook_routes_are_in_the_manifest_and_are_authenticated() {
    let hooks: Vec<&ghostai_server::manifest::Route> = ROUTE_MANIFEST
        .iter()
        .filter(|route| route.path.starts_with("/api/_test/"))
        .collect();
    assert_eq!(hooks.len(), 5);
    // In the manifest so the auth-matrix test covers them, and `Required` like
    // the routes they stand in for: a hook that skipped authentication would be
    // a hole the moment a build shipped with the feature on.
    for hook in hooks {
        assert_eq!(hook.auth, RouteAuth::Required, "{}", hook.path);
        assert_eq!(hook.method, RouteMethod::POST, "{}", hook.path);
    }
}
