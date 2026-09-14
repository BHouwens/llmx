use super::child::ExitState;
use super::child::OwnedChild;
use super::manager::ProcessManager;
use crate::backend::local::LocalBackend;
use crate::backend::Backend;
use crate::backend::Ownership;
use crate::config::loader::parse;
use crate::logging::SessionLog;
use crate::testutil::closed_port_url;
use crate::testutil::MockServer;
use std::time::Duration;
use tokio::process::Command;

/// Does a process with this pid still exist? `kill(pid, 0)` performs the
/// permission and existence checks without sending anything.
fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 sends nothing; it only probes for existence.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

async fn wait_for<F: Fn() -> bool>(condition: F, limit: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + limit;
    while tokio::time::Instant::now() < deadline {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    condition()
}

#[tokio::test]
async fn an_owned_child_is_terminated_on_shutdown() {
    let log = SessionLog::in_memory();
    let mut command = Command::new("sleep");
    command.arg("60");

    let mut child = OwnedChild::spawn("test sleeper", command, &log).expect("spawn");
    let pid = child.pid();
    assert!(pid_alive(pid), "child should be running");
    assert_eq!(child.poll().expect("poll"), ExitState::Running);

    child.shutdown().await.expect("shutdown");
    assert!(
        wait_for(|| !pid_alive(pid), Duration::from_secs(3)).await,
        "the child llmx started must be gone after shutdown"
    );
}

/// Model-server launchers are often wrappers that fork the real server.
/// Killing only the wrapper would leave the port held, so llmx signals the
/// whole process group.
#[tokio::test]
async fn shutdown_reaches_processes_the_child_forked() {
    let log = SessionLog::in_memory();
    let mut command = Command::new("sh");
    command.arg("-c").arg("sleep 60 & echo $!; wait");

    let mut child = OwnedChild::spawn("forking wrapper", command, &log).expect("spawn");

    // The wrapper prints its background child's pid on stdout.
    let grandchild: u32 = {
        let mut found = None;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while tokio::time::Instant::now() < deadline {
            if let Some(line) = child.output().snapshot().first() {
                if let Ok(pid) = line.trim().parse::<u32>() {
                    found = Some(pid);
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        found.expect("wrapper should print its child pid")
    };

    assert!(pid_alive(grandchild), "grandchild should be running");

    child.shutdown().await.expect("shutdown");

    assert!(
        wait_for(|| !pid_alive(grandchild), Duration::from_secs(5)).await,
        "the forked descendant must be cleaned up too, not left holding the port"
    );
}

#[tokio::test]
async fn a_child_that_exits_on_its_own_is_reported_with_its_output() {
    let log = SessionLog::in_memory();
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg("echo 'bind: Address already in use' >&2; exit 255");

    let child = OwnedChild::spawn("ssh tunnel (test)", command, &log).expect("spawn");
    let mut manager = ProcessManager::new();
    manager.adopt(child);

    let mut dead = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while dead.is_empty() && tokio::time::Instant::now() < deadline {
        dead = manager.check_liveness();
        if dead.is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    let dead = dead.first().expect("the exit should have been noticed");
    assert_eq!(dead.label, "ssh tunnel (test)");
    assert_eq!(dead.code, Some(255));
    assert!(
        dead.output.contains("Address already in use"),
        "the process's own words should reach the user, got: {}",
        dead.output
    );
    assert!(
        manager.is_empty(),
        "a dead child must not stay in the owned set"
    );
}

#[tokio::test]
async fn shutdown_is_safe_to_call_twice() {
    let log = SessionLog::in_memory();
    let mut command = Command::new("sleep");
    command.arg("60");

    let mut child = OwnedChild::spawn("test sleeper", command, &log).expect("spawn");
    child.shutdown().await.expect("first shutdown");
    child
        .shutdown()
        .await
        .expect("second shutdown must be a no-op");
}

#[tokio::test]
async fn the_manager_stops_everything_it_owns() {
    let log = SessionLog::in_memory();
    let mut manager = ProcessManager::new();
    let mut pids = Vec::new();

    for index in 0..3 {
        let mut command = Command::new("sleep");
        command.arg("60");
        let child = OwnedChild::spawn(format!("sleeper {index}"), command, &log).expect("spawn");
        pids.push(child.pid());
        manager.adopt(child);
    }

    assert_eq!(manager.owned_summary().len(), 3);
    manager.shutdown_all(&log).await;
    assert!(manager.is_empty());

    for pid in pids {
        assert!(
            wait_for(|| !pid_alive(pid), Duration::from_secs(3)).await,
            "pid {pid} should be gone"
        );
    }
}

// ------------------------------------------------------- ownership rules --

fn local_profile(url: &str, command: &str) -> crate::config::ModelProfile {
    let text = format!(
        r#"
[[models]]
id = "under-test"
name = "Under Test"
backend = "local"
[models.endpoint]
url = "{url}"
[models.runtime]
command = "{command}"
args = ["60"]
"#
    );
    parse(&text).expect("fixture parses").models.remove(0)
}

/// The invariant that matters most: a model server someone else started must
/// survive llmx entirely — llmx must neither spawn a duplicate nor signal it.
#[tokio::test]
async fn an_already_running_endpoint_is_reused_and_left_alone() {
    let server = MockServer::json(r#"{"data":[{"id":"already-loaded"}]}"#).await;
    let log = SessionLog::in_memory();
    let profile = local_profile(&server.base_url(), "sleep");

    let mut backend = LocalBackend::new(&profile, Duration::from_secs(5), log.clone());
    backend.start().await.expect("start should succeed");

    assert!(
        !backend.owns_process(),
        "llmx must not claim ownership of a server it did not start"
    );
    assert_eq!(backend.ownership(), Ownership::External);

    backend.shutdown().await.expect("shutdown");

    // The definitive check: the external server still answers afterwards.
    assert!(
        crate::endpoint::health::is_reachable(&server.base_url(), None).await,
        "an externally started server must still be running after llmx exits"
    );
}

#[tokio::test]
async fn a_server_llmx_started_is_owned_and_stopped() {
    let url = closed_port_url().await;
    let log = SessionLog::in_memory();
    let profile = local_profile(&url, "sleep");

    let mut backend = LocalBackend::new(&profile, Duration::from_secs(1), log.clone());
    backend
        .start()
        .await
        .expect("start should spawn the runtime");

    assert!(backend.owns_process(), "llmx started it, so llmx owns it");
    assert_eq!(backend.ownership(), Ownership::Owned);

    backend.shutdown().await.expect("shutdown");
    assert!(
        !backend.owns_process(),
        "nothing should remain owned after shutdown"
    );
}

#[tokio::test]
async fn a_local_profile_with_no_runtime_says_what_is_missing() {
    let url = closed_port_url().await;
    let text = format!(
        r#"
[[models]]
id = "no-runtime"
name = "No Runtime"
backend = "local"
[models.endpoint]
url = "{url}"
"#
    );
    let profile = parse(&text).expect("parses").models.remove(0);
    let log = SessionLog::in_memory();

    let mut backend = LocalBackend::new(&profile, Duration::from_secs(1), log);
    let err = format!("{:#}", backend.start().await.expect_err("should fail"));

    assert!(err.contains("[models.runtime]"), "got: {err}");
    assert!(err.contains("No Runtime"), "should name the profile: {err}");
    assert!(!backend.owns_process());
}

#[tokio::test]
async fn a_runtime_command_that_does_not_exist_is_reported_clearly() {
    let url = closed_port_url().await;
    let profile = local_profile(&url, "llmx-no-such-binary");
    let log = SessionLog::in_memory();

    let mut backend = LocalBackend::new(&profile, Duration::from_secs(1), log);
    let err = format!("{:#}", backend.start().await.expect_err("should fail"));

    assert!(err.contains("llmx-no-such-binary"), "got: {err}");
    assert!(err.contains("PATH"), "should hint at the cause: {err}");
    assert!(!backend.owns_process());
}

/// A backend whose process dies must fail fast with the process's own error
/// text, rather than spending the whole health timeout retrying a connection
/// that can never succeed.
#[tokio::test]
async fn a_dying_backend_aborts_the_wait_with_its_error_text() {
    let url = closed_port_url().await;
    let text = format!(
        r#"
[[models]]
id = "doomed"
name = "Doomed"
backend = "local"
[models.endpoint]
url = "{url}"
[models.runtime]
command = "sh"
args = ["-c", "echo 'CUDA out of memory' >&2; exit 1"]
"#
    );
    let profile = parse(&text).expect("parses").models.remove(0);
    let log = SessionLog::in_memory();

    let mut backend = LocalBackend::new(&profile, Duration::from_secs(60), log);
    backend.start().await.expect("spawn");

    let (_tx, mut cancel) = tokio::sync::watch::channel(false);
    let mut noop = |_: crate::endpoint::WaitProgress| {};

    let started = tokio::time::Instant::now();
    let err = backend
        .wait_ready(&mut cancel, &mut noop)
        .await
        .expect_err("the runtime died, so this cannot succeed");
    let elapsed = started.elapsed();

    let message = err.to_string();
    assert!(
        message.contains("CUDA out of memory"),
        "the runtime's own error should surface: {message}"
    );
    assert!(
        elapsed < Duration::from_secs(15),
        "must not wait out the 60s timeout for a process that already exited (took {elapsed:?})"
    );
}
