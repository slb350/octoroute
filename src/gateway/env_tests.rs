use super::env::{DotenvEnvironment, DotenvLoadError, Environment};
use secrecy::{ExposeSecret, SecretString};
use std::{collections::BTreeMap, path::Path};
use tempfile::NamedTempFile;

#[derive(Debug, Default)]
struct TestEnvironment {
    values: BTreeMap<String, String>,
}

impl TestEnvironment {
    fn with(mut self, name: &str, value: &str) -> Self {
        self.values.insert(name.to_string(), value.to_string());
        self
    }
}

impl Environment for TestEnvironment {
    fn get(&self, name: &str) -> Option<SecretString> {
        self.values.get(name).cloned().map(SecretString::from)
    }
}

fn dotenv_file(contents: &str) -> NamedTempFile {
    let file = NamedTempFile::new().expect("temporary dotenv file");
    std::fs::write(file.path(), contents).expect("write dotenv fixture");
    file
}

#[test]
fn dotenv_values_are_available_without_mutating_process_environment() {
    let file = dotenv_file(
        r#"
OPENROUTER_API_KEY="file-openrouter-key"
OCTOROUTE_API_KEY=file-inbound-key
"#,
    );
    let environment = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect("valid dotenv file");

    assert_eq!(
        environment
            .get("OPENROUTER_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("file-openrouter-key")
    );
    assert_eq!(
        environment
            .get("OCTOROUTE_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("file-inbound-key")
    );
}

#[test]
fn parent_environment_overrides_dotenv_file() {
    let file = dotenv_file("OPENROUTER_API_KEY=file-key\n");
    let parent = TestEnvironment::default().with("OPENROUTER_API_KEY", "process-key");
    let environment = DotenvEnvironment::from_path(file.path(), parent).expect("valid dotenv file");

    assert_eq!(
        environment
            .get("OPENROUTER_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("process-key")
    );
}

#[test]
fn optional_missing_dotenv_file_uses_parent_environment() {
    let parent = TestEnvironment::default().with("OPENROUTER_API_KEY", "process-key");
    let environment =
        DotenvEnvironment::from_optional_path(Path::new("definitely-absent.env"), parent)
            .expect("missing optional dotenv file");

    assert_eq!(
        environment
            .get("OPENROUTER_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("process-key")
    );
}

#[test]
fn malformed_dotenv_does_not_expose_the_line_value() {
    let file = dotenv_file("OPENROUTER_API_KEY='private-value\n");
    let error = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect_err("malformed dotenv must fail");

    assert!(matches!(error, DotenvLoadError::Invalid { .. }));
    assert!(!error.to_string().contains("private-value"));
}
