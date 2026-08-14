use std::{fs, process::Command};
use tempfile::tempdir;

const LABELED_FORECASTS: &str = concat!(
    "{\"challenge_id\":\"easy\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"octoroute-strix-capability-card/v1\",\"capability_card_fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"p_local_success\":0.9,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}\n",
    "{\"challenge_id\":\"hard\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"octoroute-strix-capability-card/v1\",\"capability_card_fingerprint\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\",\"p_local_success\":0.1,\"capability_boundary\":\"unsupported\",\"primary_rule\":\"known_local_limit\",\"local_success\":false}\n"
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
