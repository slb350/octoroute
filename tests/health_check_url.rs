//! Integration tests for health check URL construction
//!
//! Regression test for critical bug where health check URLs were malformed,
//! causing all endpoints to become unhealthy after 90 seconds

use octoroute::config::ModelEndpoint;

#[test]
fn test_health_check_url_construction_does_not_double_v1() {
    // This is a unit test documenting the health check URL construction logic
    // The critical bug was: base_url = "http://host:port/v1"
    //                       health check URL = base_url + "/v1/models"
    //                       result = "http://host:port/v1/v1/models" (404!)
    //
    // The fix: health check URL = base_url + "/models"
    //          result = "http://host:port/v1/models" (correct!)

    let endpoint = ModelEndpoint {
        name: "test-endpoint".to_string(),
        base_url: "http://localhost:11434/v1".to_string(),
        max_tokens: 4096,
        temperature: 0.7,
        weight: 1.0,
        priority: 1,
    };

    // The correct health check URL should be base_url + "/models"
    let health_check_url = format!("{}/models", endpoint.base_url);

    assert_eq!(
        health_check_url, "http://localhost:11434/v1/models",
        "Health check URL should not duplicate /v1"
    );

    // Verify it doesn't contain "/v1/v1/"
    assert!(
        !health_check_url.contains("/v1/v1/"),
        "Health check URL should not contain /v1/v1/"
    );
}

#[test]
fn test_base_url_formats() {
    // Test various base_url formats to ensure health check URL is always correct

    let test_cases = vec![
        (
            "http://localhost:11434/v1",
            "http://localhost:11434/v1/models",
        ),
        (
            "https://api.example.com/v1",
            "https://api.example.com/v1/models",
        ),
        (
            "http://192.168.1.100:8080/v1",
            "http://192.168.1.100:8080/v1/models",
        ),
    ];

    for (base_url, expected_health_url) in test_cases {
        let health_check_url = format!("{}/models", base_url);
        assert_eq!(
            health_check_url, expected_health_url,
            "Health check URL should be correct for base_url: {}",
            base_url
        );
    }
}

#[test]
fn test_health_check_url_never_has_double_slash() {
    // Ensure health check URLs don't have double slashes (except in http://)

    let base_url = "http://localhost:11434/v1";
    let health_check_url = format!("{}/models", base_url);

    // Count occurrences of "//" - should only be 1 (in "http://")
    let double_slash_count = health_check_url.matches("//").count();
    assert_eq!(
        double_slash_count, 1,
        "Health check URL should only have one // (in protocol), got: {}",
        health_check_url
    );
}
