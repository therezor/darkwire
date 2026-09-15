//! One warm container per session, and the refusals around it.
//!
//! No daemon anywhere: the engine is a double that records argv, so every
//! decision the pool makes — when it starts, when it reaps, what it refuses —
//! is observable without a container runtime. The one place a real `docker`
//! would be needed is covered by pointing the CLI engine at a shell script.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "a fixture that cannot load is a failing test either way"
)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ghostai_core::testkit::ManualClock;
use ghostai_core::{Clock, ErrorKind, GhostError, Result};
use ghostai_protocol::{ContainerNetwork, NetworkMode};
use ghostai_sandbox::container_pool::{
    CONTAINER_IDLE_MS, ContainerEngine, ContainerPool, ContainerPoolOptions, IdFactory,
    MAX_LIVE_CONTAINERS, OWNER_LABEL, RunnerFactory, owner_process_looks_alive, owner_tag,
};
use ghostai_security::PolicyStore;
use ghostai_tools::{BoxFuture, CommandRunner, PlacementRequest, RunOutcome, RunRequest};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tempfile::TempDir;

const DIGEST: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";

/// What a fake engine was asked to do, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Probe,
    Start(Vec<String>),
    Stop(String),
    Reap,
}

/// An engine with no daemon behind it.
struct FakeEngine {
    calls: Mutex<Vec<Call>>,
    probe_fails: Mutex<Option<String>>,
    start_fails: Mutex<Option<String>>,
    stop_fails: bool,
    reap_fails: bool,
}

impl FakeEngine {
    fn new() -> Arc<FakeEngine> {
        Arc::new(FakeEngine {
            calls: Mutex::new(Vec::new()),
            probe_fails: Mutex::new(None),
            start_fails: Mutex::new(None),
            stop_fails: false,
            reap_fails: false,
        })
    }

    fn calls(&self) -> Vec<Call> {
        self.calls.lock().clone()
    }

    fn starts(&self) -> Vec<Vec<String>> {
        self.calls
            .lock()
            .iter()
            .filter_map(|call| match call {
                Call::Start(argv) => Some(argv.clone()),
                _ => None,
            })
            .collect()
    }

    fn stops(&self) -> Vec<String> {
        self.calls
            .lock()
            .iter()
            .filter_map(|call| match call {
                Call::Stop(name) => Some(name.clone()),
                _ => None,
            })
            .collect()
    }
}

impl ContainerEngine for FakeEngine {
    fn start(&self, argv: &[String]) -> Result<()> {
        self.calls.lock().push(Call::Start(argv.to_vec()));
        match self.start_fails.lock().clone() {
            Some(reason) => Err(GhostError::new(ErrorKind::Tool, reason)),
            None => Ok(()),
        }
    }

    fn stop(&self, name: &str) -> Result<()> {
        self.calls.lock().push(Call::Stop(name.to_owned()));
        if self.stop_fails {
            return Err(GhostError::new(ErrorKind::Tool, "no such container"));
        }
        Ok(())
    }

    fn probe(&self) -> Result<()> {
        self.calls.lock().push(Call::Probe);
        match self.probe_fails.lock().clone() {
            Some(reason) => Err(GhostError::new(ErrorKind::Tool, reason)),
            None => Ok(()),
        }
    }

    fn reap_orphans(&self) -> Result<()> {
        self.calls.lock().push(Call::Reap);
        if self.reap_fails {
            return Err(GhostError::new(ErrorKind::Tool, "docker ps failed"));
        }
        Ok(())
    }
}

/// A runner that records which container each command ran in, and answers a
/// script.
///
/// The script is shared across every container the pool starts, so a test can
/// say "the next two commands report the container gone" without knowing which
/// runner will serve them.
struct FakeRunner {
    runs: Arc<Mutex<Vec<String>>>,
    outcomes: Arc<Mutex<Vec<RunOutcome>>>,
    name: String,
}

impl CommandRunner for FakeRunner {
    fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
        self.runs.lock().push(self.name.clone());
        let scripted = {
            let mut queue = self.outcomes.lock();
            if queue.is_empty() {
                None
            } else {
                Some(queue.remove(0))
            }
        };
        let _ = request;
        Box::pin(async move { Ok(scripted.unwrap_or_else(ok_outcome)) })
    }
}

fn ok_outcome() -> RunOutcome {
    RunOutcome {
        stdout: "done".to_owned(),
        stderr: String::new(),
        truncated: false,
        code: Some(0),
        signal: None,
        timed_out: false,
        transcript_dir: None,
    }
}

/// The shape a daemon reports when the container went away underneath a turn.
fn gone_outcome() -> RunOutcome {
    RunOutcome {
        stderr: "Error response from daemon: No such container: ghost-sbx-1".to_owned(),
        code: Some(1),
        ..ok_outcome()
    }
}

struct Harness {
    _temp: TempDir,
    root: PathBuf,
    runs_dir: PathBuf,
    store: Arc<PolicyStore>,
    engine: Arc<FakeEngine>,
    clock: Arc<ManualClock>,
    /// Which container each command ran in, in order.
    ran_in: Arc<Mutex<Vec<String>>>,
    /// What the next commands answer, in order, whichever container serves
    /// them. Empty means every command succeeds.
    scripted: Arc<Mutex<Vec<RunOutcome>>>,
    counter: Arc<Mutex<u64>>,
}

/// A container definition with the fields a test cares about patched in.
fn definition(name: &str, overrides: &Value) -> Value {
    let mut value = json!({
        "schema": "ghostai.container/1",
        "name": name,
        "image": DIGEST,
    });
    for (key, patch) in overrides.as_object().unwrap() {
        value[key] = patch.clone();
    }
    value
}

impl Harness {
    fn new() -> Harness {
        let temp = TempDir::new().unwrap();
        let root = temp.path().to_path_buf();
        Harness {
            _temp: temp,
            runs_dir: root.join("runs"),
            store: Arc::new(PolicyStore::new(root.clone())),
            root,
            engine: FakeEngine::new(),
            clock: Arc::new(ManualClock::at(common::NOW)),
            ran_in: Arc::new(Mutex::new(Vec::new())),
            scripted: Arc::new(Mutex::new(Vec::new())),
            counter: Arc::new(Mutex::new(0)),
        }
    }

    /// Installs a container definition and approves it.
    fn install(&self, name: &str, overrides: &Value) {
        common::write(
            &self.root.join("containers").join(format!("{name}.json")),
            serde_json::to_string(&definition(name, overrides)).unwrap(),
        );
        self.store.approve_container(name).unwrap();
    }

    /// Installs a one-operation toolbox and approves it.
    ///
    /// Nothing the pool does reads one — placement and grants are two
    /// approvals — so this exists only for the suites that prove the two
    /// lifecycles are independent.
    fn install_toolbox(&self, name: &str) {
        common::write(
            &self.root.join("tool-definitions/status.json"),
            json!({
                "schema": "ghostai.tool/1",
                "description": "Repository status",
                "implementation": {
                    "kind": "command",
                    "executable": "/usr/bin/git",
                    "argv": ["status"],
                },
                "parameters": {"type": "object", "properties": {}, "additionalProperties": false},
            })
            .to_string(),
        );
        common::write(
            &self.root.join("toolboxes").join(format!("{name}.json")),
            json!({
                "schema": "ghostai.toolbox/1",
                "name": name,
                "tools": [{"name": "status", "definition": "status", "permission": "allow"}],
            })
            .to_string(),
        );
        self.store.approve_toolbox(name).unwrap();
    }

    /// Deterministic container names, so a test can assert on one.
    fn ids(&self) -> IdFactory {
        let counter = Arc::clone(&self.counter);
        Arc::new(move || {
            let mut counter = counter.lock();
            *counter += 1;
            counter.to_string()
        })
    }

    /// A runner per container, recording which one each command ran in.
    fn runners(&self) -> RunnerFactory {
        let runs = Arc::clone(&self.ran_in);
        let scripted = Arc::clone(&self.scripted);
        Arc::new(move |name: &str, _container| {
            Arc::new(FakeRunner {
                runs: Arc::clone(&runs),
                outcomes: Arc::clone(&scripted),
                name: name.to_owned(),
            }) as Arc<dyn CommandRunner>
        })
    }

    fn options(&self) -> ContainerPoolOptions {
        let mut options = ContainerPoolOptions::new(
            Arc::clone(&self.store),
            Arc::clone(&self.engine) as Arc<dyn ContainerEngine>,
            self.runs_dir.clone(),
        );
        options.clock = Arc::clone(&self.clock) as Arc<dyn Clock>;
        options.new_id = Some(self.ids());
        options.new_runner = Some(self.runners());
        options.owner = Some("test-host:1".to_owned());
        options
    }

    fn pool(&self) -> Arc<ContainerPool> {
        ContainerPool::new(self.options())
    }
}

fn request(agent: &str, workspace: &str, session: &str, container: &str) -> PlacementRequest {
    PlacementRequest {
        agent_id: agent.to_owned(),
        workspace_id: workspace.to_owned(),
        session_key: session.to_owned(),
        toolbox: String::new(),
        container: container.to_owned(),
        network: ContainerNetwork::default(),
        workspace_root: "/ghost/workspace".to_owned(),
    }
}

/// The same request, asking for a different reach.
fn reaching(mode: NetworkMode, request: PlacementRequest) -> PlacementRequest {
    PlacementRequest {
        network: ContainerNetwork {
            mode,
            ..ContainerNetwork::default()
        },
        ..request
    }
}

fn plan() -> RunRequest {
    RunRequest {
        plan: ghostai_security::ExecPlan {
            file: "nmap".to_owned(),
            args: vec!["-sn".to_owned()],
            cwd: PathBuf::from("/ghost/workspace"),
            env: indexmap::IndexMap::new(),
            timeout_ms: 0,
            max_output_bytes: 1024,
            paths: Vec::new(),
        },
        timeout_ms: 0,
        token: tokio_util::sync::CancellationToken::new(),
        clock: Arc::new(ghostai_core::SystemClock),
        tee: None,
    }
}

async fn run(runner: &Arc<dyn CommandRunner>) -> Result<RunOutcome> {
    runner.run(plan()).await
}

/// The refusal a resolve made, or a failure naming the runner it handed back.
///
/// `common::err` cannot be used here: a runner is not `Debug`, deliberately —
/// printing one would mean printing the container it is bound to.
fn refusal(result: Result<Option<Arc<dyn CommandRunner>>>) -> GhostError {
    match result {
        Ok(_) => panic!("expected a refusal, got a runner"),
        Err(error) => error,
    }
}

#[tokio::test]
async fn shares_one_container_across_agents_and_conversations() {
    let h = Harness::new();
    h.install("shared", &json!({"shared": true}));
    h.install_toolbox("writer");
    h.install_toolbox("reader");
    let pool = h.pool();
    let mut alice = request("alice", "work", "one", "shared");
    alice.toolbox = "writer".to_owned();
    let mut bob = request("bob", "work", "two", "shared");
    bob.toolbox = "reader".to_owned();

    let first = pool.resolve_turn(&alice).unwrap().unwrap();
    run(&first).await.unwrap();
    let second = pool.resolve_turn(&bob).unwrap().unwrap();
    run(&second).await.unwrap();
    assert_eq!(pool.live().len(), 1);
    pool.release_session("one");
    assert_eq!(pool.live().len(), 1);

    // Placement and grants are two approvals. The operation layer rejects the
    // revoked writer before reaching this runner; the shared instance both
    // agents were placed in is untouched.
    h.store.revoke_toolbox("writer").unwrap();
    run(&first).await.unwrap();
    run(&second).await.unwrap();
    assert_eq!(pool.live().len(), 1);

    let separate = pool
        .resolve_turn(&request("bob", "another", "two", "shared"))
        .unwrap()
        .unwrap();
    run(&separate).await.unwrap();
    assert_eq!(pool.live().len(), 2);
}

#[tokio::test]
async fn gives_two_agents_asking_for_different_egress_two_shared_instances() {
    let h = Harness::new();
    h.install("shared", &json!({"shared": true}));
    let pool = h.pool();
    // An instance that served the wider of the two requests would quietly hand
    // the narrower one a reach nobody granted it, so the network is part of a
    // shared instance's identity.
    let walled = reaching(NetworkMode::None, request("alice", "work", "one", "shared"));
    let open = reaching(NetworkMode::Open, request("bob", "work", "two", "shared"));
    for spec in [&walled, &open] {
        let runner = pool.resolve_turn(spec).unwrap().unwrap();
        run(&runner).await.unwrap();
    }
    assert_eq!(pool.live().len(), 2);

    // A third agent asking for the reach the first one asked for joins it
    // rather than starting a third.
    let same = reaching(
        NetworkMode::None,
        request("carol", "work", "three", "shared"),
    );
    let runner = pool.resolve_turn(&same).unwrap().unwrap();
    run(&runner).await.unwrap();
    assert_eq!(pool.live().len(), 2);
}

#[tokio::test]
async fn scopes_the_transcript_directory_to_the_instance_a_request_lands_in() {
    let h = Harness::new();
    h.install("shared", &json!({"shared": true}));
    let pool = h.pool();
    let walled = reaching(NetworkMode::None, request("alice", "work", "one", "shared"));
    let open = reaching(NetworkMode::Open, request("bob", "work", "two", "shared"));
    for spec in [&walled, &open] {
        let runner = pool.resolve_turn(spec).unwrap().unwrap();
        run(&runner).await.unwrap();
    }

    // Matching on workspace and digest alone would let two shared instances of
    // one container — separate because they asked for different egress — read
    // each other's transcripts.
    let first = pool.transcript_directory(&walled).unwrap();
    let second = pool.transcript_directory(&open).unwrap();
    assert_ne!(first, second);
    assert_eq!(first, h.runs_dir.join("ghost-sbx-1"));
    assert_eq!(second, h.runs_dir.join("ghost-sbx-2"));

    // An instance that is gone has no transcripts to read, which is a refusal
    // rather than somebody else's directory.
    pool.close();
    assert_eq!(
        common::err(pool.transcript_directory(&walled)).kind,
        ErrorKind::NotFound
    );
}

#[tokio::test]
async fn explicit_stop_invalidates_queued_handles_without_replaying_commands() {
    let h = Harness::new();
    h.install("shared", &json!({"shared": true}));
    let pool = h.pool();
    let spec = request("alice", "work", "one", "shared");
    let runner = pool.resolve_turn(&spec).unwrap().unwrap();
    run(&runner).await.unwrap();
    pool.stop_instance(&pool.live()[0], false).unwrap();
    assert!(run(&runner).await.is_err());
    let fresh = pool.resolve_turn(&spec).unwrap().unwrap();
    run(&fresh).await.unwrap();
    assert_eq!(pool.live().len(), 1);
}

#[tokio::test]
async fn does_not_touch_the_container_runtime_until_a_turn_needs_one() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    // Building the pool probes nothing: an install with a sandboxed agent must
    // still boot with the daemon closed.
    assert!(h.engine.calls().is_empty());
    let runner = pool.resolve_turn(&request("a", "w", "s", "dev")).unwrap();
    // And opening the turn still probes nothing — only a command does.
    assert!(runner.is_some());
    assert!(h.engine.calls().is_empty());
}

#[tokio::test]
async fn sweeps_containers_a_previous_process_left_behind_on_first_use() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    assert!(h.engine.calls().contains(&Call::Reap));
    // Once, not per command: a sweep is a `docker ps` and the pool is warm now.
    run(&runner).await.unwrap();
    assert_eq!(
        h.engine
            .calls()
            .iter()
            .filter(|call| **call == Call::Reap)
            .count(),
        1
    );
}

#[tokio::test]
async fn still_runs_the_turn_when_the_sweep_fails() {
    let mut engine = FakeEngine::new();
    Arc::get_mut(&mut engine).unwrap().reap_fails = true;
    let h = Harness {
        engine,
        ..Harness::new()
    };
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    // An orphan nobody could remove is untidy; refusing the turn over it would
    // turn untidy into unusable.
    assert!(run(&runner).await.is_ok());
}

#[tokio::test]
async fn returns_no_runner_for_an_agent_that_names_no_container() {
    let h = Harness::new();
    let pool = h.pool();
    // Not a refusal: a request that selects no container is the host.
    assert!(
        pool.resolve_turn(&request("a", "w", "s", ""))
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn refuses_a_container_that_was_never_approved() {
    let h = Harness::new();
    common::write(
        &h.root.join("containers/dev.json"),
        serde_json::to_string(&definition("dev", &json!({}))).unwrap(),
    );
    let pool = h.pool();
    // **Never a downgrade to the host.** `Ok(None)` would mean "run it here",
    // so a container that cannot be honoured is an error rather than an absent
    // runner.
    let error = refusal(pool.resolve_turn(&request("a", "w", "s", "dev")));
    assert_eq!(error.kind, ErrorKind::Config);
    assert!(
        error.message.contains("never been approved"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn allows_a_sandbox_without_starting_one_then_starts_it_on_the_first_command() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    assert!(pool.live().is_empty());
    run(&runner).await.unwrap();
    assert_eq!(pool.live(), vec!["ghost-sbx-1".to_owned()]);
    assert_eq!(*h.ran_in.lock(), vec!["ghost-sbx-1".to_owned()]);
}

#[tokio::test]
async fn opens_a_turn_without_a_daemon_and_fails_only_the_command() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    *h.engine.probe_fails.lock() = Some("cannot connect to the daemon".to_owned());
    let pool = h.pool();

    // The turn opens: a daemon that is down surfaces as a failed tool card
    // inside a live turn rather than a refusal with no turn to belong to.
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    let error = common::err(run(&runner).await);
    assert_eq!(error.kind, ErrorKind::Tool);
    assert!(
        error.message.contains("No container runtime is reachable"),
        "{}",
        error.message
    );
    assert_eq!(error.details["agentId"], "a");
    assert!(pool.live().is_empty());
}

#[tokio::test]
async fn starts_the_container_once_the_daemon_comes_back() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    *h.engine.probe_fails.lock() = Some("down".to_owned());
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    assert!(run(&runner).await.is_err());
    *h.engine.probe_fails.lock() = None;
    // The failure was not remembered, so the operator starting the daemon is
    // enough.
    assert!(run(&runner).await.is_ok());
    assert_eq!(pool.live().len(), 1);
}

#[tokio::test]
async fn refuses_rather_than_falling_back_to_the_host_when_the_engine_fails() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    *h.engine.start_fails.lock() = Some("no such image: sha256:dddd".to_owned());
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    let error = common::err(run(&runner).await);
    assert_eq!(error.kind, ErrorKind::Tool);
    // The daemon's own words, rather than a bare "could not be started" that
    // sends the reader to the logs for the one fact that would have helped.
    assert!(error.message.contains("no such image"), "{}", error.message);
    assert_eq!(error.details["container"], "dev");
    assert!(h.ran_in.lock().is_empty(), "nothing ran on the host");
}

#[tokio::test]
async fn refuses_an_allow_list_with_nothing_in_it() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    // An allow-list that reaches nothing is a mode chosen by mistake, and the
    // pool says so on the turn rather than starting a container around it.
    let asking = reaching(NetworkMode::Allowlist, request("a", "w", "s", "dev"));
    assert_eq!(refusal(pool.resolve_turn(&asking)).kind, ErrorKind::Config);
}

#[tokio::test]
async fn reuses_the_container_for_a_second_turn_in_the_same_session() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    let first = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&first).await.unwrap();
    let second = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&second).await.unwrap();
    assert_eq!(pool.live().len(), 1);
    // One start, and both turns ran in the container it made.
    assert_eq!(h.engine.starts().len(), 1);
    assert_eq!(
        *h.ran_in.lock(),
        vec!["ghost-sbx-1".to_owned(), "ghost-sbx-1".to_owned()]
    );
}

#[tokio::test]
async fn gives_two_sessions_two_containers() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    for session in ["s1", "s2"] {
        let runner = pool
            .resolve_turn(&request("a", "w", session, "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    // One engagement's loot must not sit in another's `/tmp`.
    assert_eq!(pool.live().len(), 2);
}

#[tokio::test]
async fn gives_two_workspaces_two_containers_because_the_mount_differs() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    for workspace in ["w1", "w2"] {
        let runner = pool
            .resolve_turn(&request("a", workspace, "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    assert_eq!(pool.live().len(), 2);
}

#[tokio::test]
async fn gives_two_agents_two_containers_because_a_private_one_is_theirs_alone() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    for agent in ["a1", "a2"] {
        let runner = pool
            .resolve_turn(&request(agent, "w", "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    assert_eq!(pool.live().len(), 2);
}

#[tokio::test]
async fn stops_a_container_that_has_gone_idle() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    assert_eq!(pool.live().len(), 1);

    h.clock.advance(Duration::from_millis(
        u64::try_from(CONTAINER_IDLE_MS).unwrap() + 1,
    ));
    // The sweep runs on the next turn to ask for a runner.
    pool.resolve_turn(&request("b", "w", "s2", "dev")).unwrap();
    assert!(pool.live().is_empty());
    assert_eq!(h.engine.stops(), vec!["ghost-sbx-1".to_owned()]);
}

#[tokio::test]
async fn a_zero_idle_window_disables_the_sweep_rather_than_reaping_everything() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let mut options = h.options();
    options.idle_ms = 0;
    let pool = ContainerPool::new(options);
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    pool.resolve_turn(&request("a", "w", "s", "dev")).unwrap();
    assert_eq!(pool.live().len(), 1);
}

#[tokio::test]
async fn evicts_the_least_recently_used_beyond_the_cap() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let mut options = h.options();
    options.max_live = 2;
    let pool = ContainerPool::new(options);
    for session in ["s1", "s2", "s3"] {
        let runner = pool
            .resolve_turn(&request("a", "w", session, "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    assert_eq!(pool.live().len(), 2);
    assert_eq!(h.engine.stops(), vec!["ghost-sbx-1".to_owned()]);
}

#[tokio::test]
async fn a_cap_of_one_still_hands_back_the_container_it_started() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let mut options = h.options();
    // The smallest cap that permits a container at all, and the boundary the
    // cap check is written against: at one, the first turn is already at it.
    options.max_live = 1;
    let pool = ContainerPool::new(options);
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    assert_eq!(pool.live().len(), 1);
    assert_eq!(*h.ran_in.lock(), vec!["ghost-sbx-1".to_owned()]);
}

#[tokio::test]
async fn a_zero_cap_means_no_cap_rather_than_no_containers() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let mut options = h.options();
    // The same reading `idle_ms` gets. An operator who writes zero means "do
    // not bound this"; taking it literally would refuse every container and
    // leave nothing to say why.
    options.max_live = 0;
    let pool = ContainerPool::new(options);
    for session in 0..=MAX_LIVE_CONTAINERS {
        let runner = pool
            .resolve_turn(&request("a", "w", &session.to_string(), "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    assert_eq!(pool.live().len(), MAX_LIVE_CONTAINERS + 1);
    assert!(h.engine.stops().is_empty());
}

#[tokio::test]
async fn stops_every_container_a_session_owns_when_it_ends() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    for agent in ["a1", "a2"] {
        let runner = pool
            .resolve_turn(&request(agent, "w", "chat", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    let other = pool
        .resolve_turn(&request("a1", "w", "other", "dev"))
        .unwrap()
        .unwrap();
    run(&other).await.unwrap();

    pool.release_session("chat");
    assert_eq!(pool.live().len(), 1);
}

#[tokio::test]
async fn releases_only_the_session_it_was_asked_about() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    // A key that *ends with* another's text must not be swept with it, which is
    // why the session is matched on the whole trailing field.
    for session in ["chat", "not chat"] {
        let runner = pool
            .resolve_turn(&request("a", "w", session, "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    pool.release_session("chat");
    assert_eq!(pool.live().len(), 1);
}

#[tokio::test]
async fn stops_everything_on_close() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    for session in ["s1", "s2"] {
        let runner = pool
            .resolve_turn(&request("a", "w", session, "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
    }
    pool.close();
    assert!(pool.live().is_empty());
    assert_eq!(h.engine.stops().len(), 2);
}

#[tokio::test]
async fn survives_an_engine_that_cannot_stop_a_container() {
    let mut engine = FakeEngine::new();
    Arc::get_mut(&mut engine).unwrap().stop_fails = true;
    let h = Harness {
        engine,
        ..Harness::new()
    };
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    // A container that is already gone is the common case, and a failure to stop
    // one must not take down the turn that triggered the sweep.
    pool.close();
    assert!(pool.live().is_empty());
}

#[tokio::test]
async fn labels_the_container_with_its_session_definition_and_owning_process() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let pool = h.pool();
    let runner = pool
        .resolve_turn(&request("a", "w", "chat", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();
    let argv = h.engine.starts().remove(0).join(" ");
    assert!(argv.contains("ghostai.session=chat"), "{argv}");
    assert!(argv.contains("ghostai.container=dev"), "{argv}");
    assert!(
        argv.contains(&format!("{OWNER_LABEL}=test-host:1")),
        "{argv}"
    );
}

#[tokio::test]
async fn translates_every_mount_for_a_containerised_ghostai() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    let mut options = h.options();
    // A bind path is resolved by the *daemon*, so asking for GhostAI's own
    // `/ghost/...` would mount the host's path of that name — silently, and
    // usually as an empty directory.
    options.host_path = Some(Arc::new(|path: &str| format!("/host{path}")));
    let pool = ContainerPool::new(options);
    let runner = pool
        .resolve_turn(&request("a", "w", "s", "dev"))
        .unwrap()
        .unwrap();
    run(&runner).await.unwrap();

    let argv = h.engine.starts().remove(0).join(" ");
    assert!(argv.contains("/host/ghost/workspace"), "{argv}");
    // Every path, not just the workspace: the transcripts are mounted too, and
    // a container that starts reading the wrong directory is worse than one
    // that refuses.
    assert!(
        argv.contains(&format!("/host{}", h.runs_dir.display())),
        "{argv}"
    );
    assert!(!argv.contains(" /ghost/workspace"), "{argv}");
}

#[tokio::test]
async fn masks_the_host_identifying_corners_of_sysfs_only_where_that_works() {
    let h = Harness::new();
    h.install("dev", &json!({}));
    for (bin, masked) in [("docker", true), ("podman", false)] {
        let mut options = h.options();
        options.bin = Some(bin.to_owned());
        let pool = ContainerPool::new(options);
        let runner = pool
            .resolve_turn(&request("a", "w", bin, "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
        // Podman copies the underlying sysfs directory up into the tmpfs laid
        // over it, which needs a capability this container does not hold.
        let argv = h.engine.starts().pop().unwrap().join(" ");
        assert_eq!(argv.contains("--tmpfs=/sys/"), masked, "{bin}: {argv}");
    }
}

mod a_container_with_a_command_in_it {
    use super::*;

    /// A runner that blocks until the test releases it.
    struct Blocking(Arc<tokio::sync::Notify>, Arc<Mutex<bool>>);

    impl CommandRunner for Blocking {
        fn run(&self, request: RunRequest) -> BoxFuture<'_, Result<RunOutcome>> {
            let _ = request;
            Box::pin(async move {
                *self.1.lock() = true;
                self.0.notified().await;
                Ok(ok_outcome())
            })
        }
    }

    fn blocking_pool(
        h: &Harness,
        max_live: usize,
    ) -> (Arc<ContainerPool>, Arc<tokio::sync::Notify>) {
        let release = Arc::new(tokio::sync::Notify::new());
        let started = Arc::new(Mutex::new(false));
        let gate = Arc::clone(&release);
        let flag = Arc::clone(&started);
        let mut options = h.options();
        options.max_live = max_live;
        options.new_runner = Some(Arc::new(move |_name: &str, _container| {
            Arc::new(Blocking(Arc::clone(&gate), Arc::clone(&flag))) as Arc<dyn CommandRunner>
        }));
        (ContainerPool::new(options), release)
    }

    #[tokio::test]
    async fn is_not_stopped_by_the_idle_sweep() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let (pool, release) = blocking_pool(&h, MAX_LIVE_CONTAINERS);
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();

        let running = tokio::spawn({
            let runner = Arc::clone(&runner);
            async move { run(&runner).await }
        });
        assert!(
            common::eventually(Duration::from_secs(5), || !pool.live().is_empty()).await,
            "the container should be up while the command runs"
        );

        // `last_used_ms` is stamped once per turn, so a scan that runs for twenty
        // minutes looks idle for nineteen of them.
        h.clock.advance(Duration::from_millis(
            u64::try_from(CONTAINER_IDLE_MS).unwrap() + 1,
        ));
        pool.resolve_turn(&request("b", "w", "s2", "dev")).unwrap();
        assert_eq!(
            pool.live().len(),
            1,
            "a container with work in it is never reaped"
        );

        release.notify_waiters();
        running.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn is_not_evicted_to_make_room_for_another_session() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let (pool, release) = blocking_pool(&h, 1);
        let busy = pool
            .resolve_turn(&request("a", "w", "s1", "dev"))
            .unwrap()
            .unwrap();
        let running = tokio::spawn({
            let busy = Arc::clone(&busy);
            async move { run(&busy).await }
        });
        assert!(common::eventually(Duration::from_secs(5), || !pool.live().is_empty()).await);

        // The second session is told to come back rather than served by
        // stopping the container the first one is scanning in: a command killed
        // to make room fails with the daemon's words and no stated reason.
        let second = pool
            .resolve_turn(&request("a", "w", "s2", "dev"))
            .unwrap()
            .unwrap();
        let error = common::err(run(&second).await);
        assert_eq!(error.kind, ErrorKind::Tool);
        assert!(
            error.message.contains("All sandbox capacity is busy"),
            "{}",
            error.message
        );
        assert_eq!(pool.live().len(), 1);

        release.notify_waiters();
        running.await.unwrap().unwrap();
    }
}

mod a_container_that_disappeared {
    use super::*;

    #[tokio::test]
    async fn is_rebuilt_and_the_command_runs_rather_than_failing() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        // The first command in the first container reports the container gone;
        // the rebuild's runner answers normally.
        *h.scripted.lock() = vec![gone_outcome()];
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();

        // A `docker exec` that could not find its container never started the
        // command, so nothing has run twice.
        let outcome = run(&runner).await.unwrap();
        assert_eq!(outcome.code, Some(0));
        assert_eq!(h.engine.starts().len(), 2);
        assert_eq!(pool.live(), vec!["ghost-sbx-2".to_owned()]);
    }

    #[tokio::test]
    async fn keeps_the_turn_on_the_same_runner_across_the_rebuild() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        *h.scripted.lock() = vec![gone_outcome()];
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();
        // A container rebuilt mid-turn is invisible to the caller, which is what
        // the indirection buys: the same handle keeps working afterwards.
        assert!(run(&runner).await.is_ok());
        assert_eq!(h.engine.starts().len(), 2);
        assert_eq!(
            *h.ran_in.lock(),
            vec![
                "ghost-sbx-1".to_owned(),
                "ghost-sbx-2".to_owned(),
                "ghost-sbx-2".to_owned(),
            ]
        );
    }

    #[tokio::test]
    async fn gives_up_after_one_rebuild() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        // Both runners report it gone: a second disappearance is something other
        // than a stale handle, and a loop that kept rebuilding would hide it.
        *h.scripted.lock() = vec![gone_outcome(), gone_outcome()];
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        let outcome = run(&runner).await.unwrap();
        assert_eq!(outcome.code, Some(1));
        assert_eq!(h.engine.starts().len(), 2);
    }

    #[tokio::test]
    async fn is_not_rebuilt_when_an_operator_stopped_it_on_purpose() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();

        // The two causes are told apart by the epoch, which a stop bumps and a
        // daemon restart does not. Rebuilding here would make the stop look
        // like it did nothing.
        pool.stop_instance(&pool.live()[0], false).unwrap();
        assert_eq!(common::err(run(&runner).await).kind, ErrorKind::Aborted);
        assert_eq!(h.engine.starts().len(), 1);
        assert!(pool.live().is_empty());
    }
}

mod approval_is_re_checked_every_turn {
    use super::*;

    #[tokio::test]
    async fn stops_reusing_a_container_once_it_is_revoked() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();

        // A revoke is another process writing a file, so nothing notifies this
        // pool; asking every turn is what makes revocation mean something.
        h.store.revoke_container("dev").unwrap();
        assert_eq!(
            refusal(pool.resolve_turn(&request("a", "w", "s", "dev"))).kind,
            ErrorKind::Config
        );
    }

    #[tokio::test]
    async fn refuses_a_command_whose_container_was_revoked_since_the_turn_opened() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        h.store.revoke_container("dev").unwrap();
        // A turn can sit between its opening and its first tool call for a long
        // time, and a container revoked in that window must not be started.
        assert_eq!(common::err(run(&runner).await).kind, ErrorKind::Config);
        assert!(pool.live().is_empty());
    }

    #[tokio::test]
    async fn replaces_a_container_whose_definition_changed_under_it() {
        let h = Harness::new();
        h.install("dev", &json!({}));
        let pool = h.pool();
        let runner = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        run(&runner).await.unwrap();

        // Edited and re-approved: the live container was built with the old
        // definition's flags, so it is stopped rather than reused.
        h.install("dev", &json!({"user": "1001:1001"}));
        let next = pool
            .resolve_turn(&request("a", "w", "s", "dev"))
            .unwrap()
            .unwrap();
        assert!(pool.live().is_empty());
        run(&next).await.unwrap();
        assert_eq!(pool.live(), vec!["ghost-sbx-2".to_owned()]);
    }
}

mod owner_liveness {
    use super::*;

    #[test]
    fn recognises_this_very_process() {
        assert!(owner_process_looks_alive(&owner_tag()));
    }

    #[test]
    fn reports_a_pid_on_this_host_that_no_longer_exists() {
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        // A pid above the kernel's maximum cannot be live.
        assert!(!owner_process_looks_alive(&format!("{host}:2147483646")));
    }

    #[test]
    fn spares_an_owner_on_another_host_which_it_cannot_ask_about() {
        // Reaping one that is live kills a command mid-flight and reports the
        // daemon's words to the model as its own failure.
        assert!(owner_process_looks_alive("some-other-host:1"));
    }

    #[test]
    fn spares_an_owner_it_cannot_parse() {
        let host = gethostname::gethostname().to_string_lossy().into_owned();
        assert!(owner_process_looks_alive("no-colon-here"));
        assert!(owner_process_looks_alive(&format!("{host}:not-a-number")));
        assert!(owner_process_looks_alive(&format!("{host}:0")));
        assert!(owner_process_looks_alive(&format!("{host}:-1")));
    }

    #[test]
    fn an_owner_tag_is_host_and_pid() {
        let tag = owner_tag();
        let (host, pid) = tag.rsplit_once(':').unwrap();
        assert!(!host.is_empty());
        assert_eq!(pid.parse::<u32>().unwrap(), std::process::id());
    }
}

#[tokio::test]
async fn describes_itself_without_naming_a_session() {
    let h = Harness::new();
    let pool = h.pool();
    let shown = format!("{pool:?}");
    assert!(shown.contains("ContainerPool"), "{shown}");
    assert!(format!("{:?}", h.options()).contains("ContainerPoolOptions"));
}
