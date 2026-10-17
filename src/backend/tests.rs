use crate::backend::ssh::ssh_hint;
use crate::backend::ssh::SshBackend;
use crate::config::loader::parse;

/// llmx runs ssh with stdin closed, so ssh can never prompt for a passphrase
/// or a host-key confirmation. Both then look like bare refusals; these hints
/// name the cause ssh cannot name itself.
#[test]
fn a_passphrase_refusal_points_at_ssh_agent() {
    let hint = ssh_hint("Permission denied (publickey).").expect("should have a hint");
    assert!(hint.contains("ssh-add"), "got: {hint}");
    assert!(hint.contains("no terminal"), "should say why: {hint}");
}

#[test]
fn an_unknown_host_key_points_at_connecting_by_hand() {
    let hint = ssh_hint("Host key verification failed.").expect("should have a hint");
    assert!(hint.contains("known_hosts"), "got: {hint}");
}

#[test]
fn a_busy_local_port_points_at_both_fields_that_must_change() {
    let detail = "bind [127.0.0.1]:8100: Address already in use\n\
                  channel_setup_fwd_listener_tcpip: cannot listen to port";
    let hint = ssh_hint(detail).expect("should have a hint");
    assert!(hint.contains("ssh.local_port"), "got: {hint}");
    assert!(
        hint.contains("endpoint.url"),
        "changing one without the other just moves the problem: {hint}"
    );
}

#[test]
fn an_unresolvable_host_points_at_doctor() {
    let hint = ssh_hint("ssh: Could not resolve hostname spark: nodename nor servname provided")
        .expect("should have a hint");
    assert!(hint.contains("doctor"), "got: {hint}");
}

#[test]
fn an_unrecognised_failure_gets_no_invented_advice() {
    assert!(
        ssh_hint("some entirely novel ssh failure").is_none(),
        "a wrong hint is worse than none"
    );
}

#[test]
fn the_ssh_command_line_is_reproducible_by_hand() {
    let text = r#"
[[models]]
id = "spark"
name = "Spark"
backend = "ssh"
[models.endpoint]
url = "http://127.0.0.1:8100/v1"
[models.ssh]
host = "spark"
local_port = 8100
remote_port = 8000
"#;
    let profile = parse(text).expect("parses").models.remove(0);
    let ssh = profile.ssh.as_ref().expect("ssh table");

    assert_eq!(
        SshBackend::command_line(ssh).join(" "),
        "ssh -N -o ExitOnForwardFailure=yes -L 8100:127.0.0.1:8000 spark",
        "`llmx explain` prints this; it must be exactly what a user could paste"
    );
}
