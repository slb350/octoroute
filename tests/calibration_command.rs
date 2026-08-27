use std::{fs, process::Command};
use tempfile::tempdir;

const LABELED_FORECASTS: &str = concat!(
    "{\"challenge_id\":\"easy\",\"model_alias\":\"local-model\",\"model_revision\":\"example-local-revision\",\"capability_card_version\":\"octoroute-local-capability-card/v2\",\"capability_card_fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"p_local_success\":0.9,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}\n",
    "{\"challenge_id\":\"hard\",\"model_alias\":\"local-model\",\"model_revision\":\"example-local-revision\",\"capability_card_version\":\"octoroute-local-capability-card/v2\",\"capability_card_fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"p_local_success\":0.1,\"capability_boundary\":\"unsupported\",\"primary_rule\":\"known_local_limit\",\"local_success\":false}\n"
);

#[test]
fn calibration_command_runs_without_gateway_config_or_credentials() {
    let directory = tempdir().expect("temporary directory");
    let input = directory.path().join("forecasts.jsonl");
    fs::write(&input, LABELED_FORECASTS).expect("write artifact");

    let output = Command::new(env!("CARGO_BIN_EXE_octoroute"))
        .args([
            "calibrate",
            "--input",
            input.to_str().expect("UTF-8 path"),
            "--grid-step",
            "0.1",
        ])
        .output()
        .expect("run calibration command");

    assert!(output.status.success());
    let report: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("calibration report JSON");
    assert_eq!(report["record_count"], 2);
    assert_eq!(report["best_accuracy"]["accuracy"], 1.0);
}

#[test]
fn calibration_command_does_not_overwrite_an_existing_report() {
    let directory = tempdir().expect("temporary directory");
    let input = directory.path().join("forecasts.jsonl");
    let report = directory.path().join("report.json");
    fs::write(&input, LABELED_FORECASTS).expect("write artifact");
    fs::write(&report, "keep-me").expect("write existing report");

    let output = Command::new(env!("CARGO_BIN_EXE_octoroute"))
        .args([
            "calibrate",
            "--input",
            input.to_str().expect("UTF-8 input path"),
            "--output",
            report.to_str().expect("UTF-8 report path"),
        ])
        .output()
        .expect("run calibration command");

    assert!(!output.status.success());
    assert_eq!(
        fs::read_to_string(report).expect("preserved report"),
        "keep-me"
    );
}

#[test]
fn calibration_command_reports_invalid_utf8_without_echoing_artifact_bytes() {
    let directory = tempdir().expect("temporary directory");
    let input = directory.path().join("invalid-utf8.jsonl");
    let mut bytes = vec![b'a'; 4096];
    bytes[0] = 0xff;
    fs::write(&input, bytes).expect("write invalid UTF-8 artifact");

    let output = Command::new(env!("CARGO_BIN_EXE_octoroute"))
        .args([
            "calibrate",
            "--input",
            input.to_str().expect("UTF-8 input path"),
        ])
        .output()
        .expect("run calibration command");

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).expect("process stderr is UTF-8");
    assert!(
        stderr.contains("forecast artifact is not valid UTF-8"),
        "{stderr}"
    );
    assert!(stderr.len() < 512, "stderr must stay bounded: {stderr}");
}
