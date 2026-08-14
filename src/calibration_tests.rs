use super::calibration::analyze_jsonl;

const LABELED_FORECASTS: &str = r#"
{"challenge_id":"easy-1","model_alias":"strixtea","capability_card_version":"octoroute-strix-capability-card/v1","p_local_success":0.9,"capability_boundary":"supported","primary_rule":"bounded_verification","local_success":true,"previous_cloud_decision":false,"cloud_success":true,"routing_latency_ms":800,"cloud_cost_usd":0.01}
{"challenge_id":"easy-2","model_alias":"strixtea","capability_card_version":"octoroute-strix-capability-card/v1","p_local_success":0.6,"capability_boundary":"uncertain","primary_rule":"ambiguous_requirements","local_success":true,"previous_cloud_decision":true,"cloud_success":true,"routing_latency_ms":900,"cloud_cost_usd":0.02}
{"challenge_id":"hard-1","model_alias":"strixtea","capability_card_version":"octoroute-strix-capability-card/v1","p_local_success":0.4,"capability_boundary":"unmatched","primary_rule":"no_matching_rule","local_success":false,"previous_cloud_decision":false,"cloud_success":true,"routing_latency_ms":1000,"cloud_cost_usd":0.03}
{"challenge_id":"hard-2","model_alias":"strixtea","capability_card_version":"octoroute-strix-capability-card/v1","p_local_success":0.1,"capability_boundary":"unsupported","primary_rule":"known_local_limit","local_success":false,"previous_cloud_decision":true,"cloud_success":false,"routing_latency_ms":1100,"cloud_cost_usd":0.04}
"#;

#[test]
fn calibration_report_covers_quality_calibration_latency_and_cost() {
    let report = analyze_jsonl(LABELED_FORECASTS, 0.1).expect("valid labeled forecasts");
    let report: serde_json::Value = serde_json::from_str(&report).expect("report JSON");

    assert_eq!(report["record_count"], 4);
    assert_eq!(report["dataset"]["model_alias"], "strixtea");
    assert_eq!(
        report["dataset"]["capability_card_version"],
        "octoroute-strix-capability-card/v1"
    );
    assert_eq!(report["calibration"]["brier_score"], 0.085);
    assert_eq!(report["always_local"]["accuracy"], 0.5);
    assert_eq!(report["previous_binary"]["accuracy"], 0.5);
    assert_eq!(report["previous_binary"]["false_escalations"], 1);
    assert_eq!(report["previous_binary"]["missed_rescues"], 1);
    assert_eq!(report["best_accuracy"]["accuracy"], 1.0);
    assert_eq!(report["best_accuracy"]["false_escalations"], 0);
    assert_eq!(report["best_accuracy"]["missed_rescues"], 0);
    assert_eq!(report["observed_average_routing_latency_ms"], 950.0);
    assert_eq!(report["candidates"].as_array().map(Vec::len), Some(36));
    assert_eq!(report["beats_always_local"], true);
}

#[test]
fn calibration_rejects_invalid_or_ambiguous_artifacts() {
    for input in [
        "",
        concat!(
            "{\"challenge_id\":\"same\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"v1\",\"p_local_success\":0.5,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}\n",
            "{\"challenge_id\":\"same\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"v1\",\"p_local_success\":0.4,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":false}\n"
        ),
        "{\"challenge_id\":\"bad\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"v1\",\"p_local_success\":1.1,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}",
        "{\"challenge_id\":\"bad\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"v1\",\"p_local_success\":0.8,\"capability_boundary\":\"unsupported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}",
        concat!(
            "{\"challenge_id\":\"one\",\"model_alias\":\"strixtea\",\"capability_card_version\":\"v1\",\"p_local_success\":0.8,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}\n",
            "{\"challenge_id\":\"two\",\"model_alias\":\"other\",\"capability_card_version\":\"v1\",\"p_local_success\":0.8,\"capability_boundary\":\"supported\",\"primary_rule\":\"bounded_verification\",\"local_success\":true}\n"
        ),
    ] {
        assert!(analyze_jsonl(input, 0.1).is_err());
    }
    assert!(analyze_jsonl(LABELED_FORECASTS, 0.0).is_err());
    assert!(analyze_jsonl(LABELED_FORECASTS, 0.3).is_err());
}
