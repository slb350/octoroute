//! Codex CLI process-level tests: the hardened invocation's deadline, its
//! bounded captures, and the failures the readiness probe has to tell apart.
//!
//! Every fixture here drives a shell script, so the whole module is Unix-only.

use super::*;
use crate::gateway::fabric::test_support::write_executable_fixture as fake_codex;

fn codex_request(executable: &Path, timeout: Duration) -> CodexRequest {
    CodexRequest {
        executable: PathBuf::from(executable),
        environment: ChildEnvironment::current(),
        model: "gpt-test".to_string(),
        effort: ReasoningEffort::High,
        timeout,
        input: "{\"contract\":\"openai_chat_completion\"}".to_string(),
        stream: false,
    }
}

#[cfg(test)]
mod process_assertions {
    pub(super) fn process_is_alive(pid: &str) -> bool {
        let listed = std::process::Command::new("ps")
            .args(["-p", pid, "-o", "pid="])
            .output()
            .expect("ps");
        !String::from_utf8_lossy(&listed.stdout).trim().is_empty()
    }
}

/// The timeout has to kill the child, not just abandon it. A CLI that never
/// answers would otherwise keep running - holding its workspace, its share of
/// the machine, and any subscription work it started - after Octoroute has
/// already answered the caller.
///
/// The fixture records its own pid, and the deadline `run_process` is given is
/// also that fixture's startup budget: on a loaded machine the shell may not
/// have been scheduled by the time the deadline fires, leaving no pid to check.
/// The budget is therefore an order of magnitude above the observed spawn
/// latency, and a run that still never started is retried rather than read from
/// a file that was never written.
#[tokio::test]
async fn the_deadline_kills_and_reaps_a_child_that_never_answers() {
    const ATTEMPTS: u32 = 3;
    const DEADLINE: Duration = Duration::from_secs(3);

    let directory = tempfile::tempdir().expect("fixture directory");
    let mut pid = None;
    for attempt in 0..ATTEMPTS {
        let pid_file = directory.path().join(format!("child-{attempt}.pid"));
        let executable = fake_codex(
            directory.path(),
            &format!("hung-codex-{attempt}"),
            // `exec` keeps the recorded pid as the process Octoroute holds, so
            // the assertion below is about the child itself and not a shell
            // wrapper.
            &format!(
                "#!/bin/sh\nprintf '%s' \"$$\" > {}\nexec sleep 30\n",
                pid_file.display()
            ),
        );

        let started = std::time::Instant::now();
        let result = run_process(
            &executable,
            &[],
            &ChildEnvironment::current(),
            directory.path(),
            b"",
            DEADLINE,
            1024,
        )
        .await;
        let elapsed = started.elapsed();

        assert!(matches!(result, Err(CodexAdapterError::Timeout)));
        assert!(
            elapsed < DEADLINE * 4,
            "the deadline must cut the run short; took {elapsed:?}"
        );
        if let Ok(recorded) = std::fs::read_to_string(&pid_file) {
            pid = Some(recorded);
            break;
        }
    }
    let pid = pid.expect("the fixture never started inside its budget in three attempts");

    // `run_process` awaits the kill and the reap before returning, so this is a
    // settled state rather than a race: a surviving `sleep 30` here means the
    // timeout path let the child outlive the request.
    assert!(
        !process_assertions::process_is_alive(pid.trim()),
        "the child must be killed and reaped on the timeout path"
    );
}

/// Capture is bounded, and the bound is enforced rather than merely documented:
/// a chatty or hostile CLI must not be buffered without limit.
#[tokio::test]
async fn output_beyond_the_capture_bound_is_refused() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let executable = fake_codex(
        directory.path(),
        "loud-codex",
        "#!/bin/sh\nhead -c 65536 /dev/zero | tr '\\0' 'x'\n",
    );

    let refused = run_process(
        &executable,
        &[],
        &ChildEnvironment::current(),
        directory.path(),
        b"",
        Duration::from_secs(30),
        1024,
    )
    .await;
    assert!(matches!(refused, Err(CodexAdapterError::OutputTooLarge)));

    // The control: the same output under a bound that admits it is returned
    // whole, so the rejection above is the bound and not the fixture.
    let admitted = run_process(
        &executable,
        &[],
        &ChildEnvironment::current(),
        directory.path(),
        b"",
        Duration::from_secs(30),
        128 * 1024,
    )
    .await
    .expect("output within the bound");
    assert_eq!(admitted.stdout.len(), 65536);
}

/// The bound has to hold while the child is still running.
///
/// A `codex exec` that streams and never finishes is the case the bound exists
/// for, and the post-exit check cannot reach it: with only that check, this
/// fixture writes for the whole timeout window and is then reported as a
/// timeout rather than refused. The fixture writes from a shell builtin loop
/// with no exit, so nothing but the in-flight check can stop it.
#[tokio::test]
async fn a_child_that_streams_without_exiting_is_cut_at_the_capture_bound() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let executable = fake_codex(
        directory.path(),
        "endless-codex",
        "#!/bin/sh\nwhile :; do printf '%s' 'xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx'; done\n",
    );

    let started = std::time::Instant::now();
    let refused = run_process(
        &executable,
        &[],
        &ChildEnvironment::current(),
        directory.path(),
        b"",
        // Long enough that a run reaching the timeout is unambiguous: the only
        // way to fail fast here is the in-flight bound.
        Duration::from_secs(10),
        1024,
    )
    .await;
    let elapsed = started.elapsed();

    assert!(
        matches!(refused, Err(CodexAdapterError::OutputTooLarge)),
        "a child that never exits must be refused by the bound, not the deadline"
    );
    assert!(
        // Loose against the fixture's startup latency on a loaded machine and
        // still far inside the deadline, which is the thing being ruled out.
        elapsed < Duration::from_secs(4),
        "the bound is checked while the child runs; took {elapsed:?}"
    );
}

/// Set a capture's apparent size without writing 16 MiB of anything.
fn capture_of_size(bytes: u64) -> BoundedCapture {
    let capture = BoundedCapture::new().expect("capture file");
    capture.file.set_len(bytes).expect("capture size");
    capture
}

/// Both captures are bounded, and stderr is the half no test reached: nothing
/// downstream reads it, so an unbounded stderr is a child writing to the disk
/// where no one will ever look. `read_bounded` only ever runs on stdout, so
/// deleting the stderr clause is invisible to every other test here.
#[test]
fn either_capture_outgrowing_its_bound_refuses_the_run() {
    let stdout_limit = 1024;

    assert!(
        !capture_exceeded(
            &capture_of_size(stdout_limit as u64),
            &capture_of_size(STDERR_CAPTURE_MAX_BYTES as u64),
            stdout_limit
        ),
        "a capture exactly at its bound is within it"
    );
    assert!(
        capture_exceeded(
            &capture_of_size(stdout_limit as u64 + 1),
            &capture_of_size(0),
            stdout_limit
        ),
        "stdout past its bound refuses the run"
    );
    assert!(
        capture_exceeded(
            &capture_of_size(0),
            &capture_of_size(STDERR_CAPTURE_MAX_BYTES as u64 + 1),
            stdout_limit
        ),
        "stderr past its own bound refuses the run too"
    );
}

/// A CLI that fails and a CLI that is not installed are different operator
/// problems, and the adapter has to tell them apart.
#[tokio::test]
async fn a_failing_exit_and_a_missing_executable_are_distinct_failures() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let failing = fake_codex(
        directory.path(),
        "failing-codex",
        "#!/bin/sh\ncat >/dev/null\nexit 3\n",
    );
    assert!(matches!(
        execute(codex_request(&failing, Duration::from_secs(30))).await,
        Err(CodexAdapterError::Process)
    ));

    let missing = directory.path().join("no-such-codex");
    assert!(matches!(
        execute(codex_request(&missing, Duration::from_secs(30))).await,
        Err(CodexAdapterError::Missing)
    ));
}

/// A run that reports its own failure part-way through is rejected even though
/// the CLI exits cleanly. Without this the caller would receive whatever
/// partial output the failed turn produced.
#[tokio::test]
async fn a_turn_that_fails_mid_run_is_rejected_despite_a_clean_exit() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let executable = fake_codex(
        directory.path(),
        "failing-turn-codex",
        concat!(
            "#!/bin/sh\n",
            "cat >/dev/null\n",
            "printf '%s\\n' \\\n",
            "  '{\"type\":\"thread.started\"}' \\\n",
            "  '{\"type\":\"turn.started\"}' \\\n",
            "  '{\"type\":\"turn.failed\",\"error\":{\"message\":\"upstream refused\"}}'\n",
            "exit 0\n"
        ),
    );

    assert!(matches!(
        execute(codex_request(&executable, Duration::from_secs(30))).await,
        Err(CodexAdapterError::Process)
    ));
}

/// The readiness probe has the same three process-level failures, and each has
/// to keep its own identity: `Process` and `Timeout` are outages, `Diagnostic`
/// is a misconfigured CLI.
#[tokio::test]
async fn doctor_probe_failures_keep_their_kind() {
    let directory = tempfile::tempdir().expect("fixture directory");
    let environment = ChildEnvironment::current();

    let failing = fake_codex(directory.path(), "doctor-exit", "#!/bin/sh\nexit 4\n");
    assert!(matches!(
        probe(&failing, &environment, Duration::from_secs(30)).await,
        Err(CodexAdapterError::Process)
    ));

    let hung = fake_codex(
        directory.path(),
        "doctor-hang",
        "#!/bin/sh\nexec sleep 30\n",
    );
    let started = std::time::Instant::now();
    assert!(matches!(
        probe(&hung, &environment, Duration::from_millis(250)).await,
        Err(CodexAdapterError::Timeout)
    ));
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "the probe deadline must cut the run short"
    );

    let unparseable = fake_codex(
        directory.path(),
        "doctor-garbage",
        "#!/bin/sh\nprintf '%s' 'not json'\n",
    );
    assert!(matches!(
        probe(&unparseable, &environment, Duration::from_secs(30)).await,
        Err(CodexAdapterError::Diagnostic)
    ));
}
