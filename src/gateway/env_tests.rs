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

/// An exported-but-empty process variable is the shape a shell leaves after
/// `export KEY=` or a failed substitution. Treating it as a value shadows a
/// good `.env` entry and surfaces as a missing credential.
#[test]
fn empty_process_variable_does_not_shadow_a_dotenv_value() {
    let file = dotenv_file("OPENROUTER_API_KEY=file-key\n");
    let parent = TestEnvironment::default().with("OPENROUTER_API_KEY", "");
    let environment = DotenvEnvironment::from_path(file.path(), parent).expect("valid dotenv file");

    assert_eq!(
        environment
            .get("OPENROUTER_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("file-key")
    );
}

/// `KEY=` in a `.env` is the same shape as an exported-but-empty process
/// variable: a truncated edit or a failed substitution, not a credential. Kept
/// as an empty secret it surfaces downstream as an upstream 401 ("invalid
/// credential") instead of the startup-visible "missing credential" it is.
#[test]
fn empty_dotenv_values_are_not_values() {
    let file = dotenv_file("OPENROUTER_API_KEY=\nOCTOROUTE_API_KEY=inbound\n");
    let environment = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect("valid dotenv file");

    assert!(
        environment.get("OPENROUTER_API_KEY").is_none(),
        "an empty dotenv assignment must not resolve to an empty secret"
    );
    assert_eq!(
        environment
            .get("OCTOROUTE_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("inbound"),
        "a neighbouring value is unaffected"
    );
}

/// A repeated key resolves to the last assignment. An operator rotating a
/// credential appends the new line; first-wins would keep serving the stale one
/// with nothing to show for it.
#[test]
fn a_repeated_dotenv_key_resolves_to_the_last_assignment() {
    let file = dotenv_file("OPENROUTER_API_KEY=stale\nOPENROUTER_API_KEY=rotated\n");
    let environment = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect("valid dotenv file");

    assert_eq!(
        environment
            .get("OPENROUTER_API_KEY")
            .as_ref()
            .map(ExposeSecret::expose_secret),
        Some("rotated")
    );
}

/// Last-wins and "empty is not a value" compose: a later blank assignment
/// clears the key rather than leaving the earlier value in place, so the two
/// rules cannot disagree about which line is authoritative.
#[test]
fn a_later_empty_assignment_clears_an_earlier_dotenv_value() {
    let file = dotenv_file("OPENROUTER_API_KEY=stale\nOPENROUTER_API_KEY=\n");
    let environment = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect("valid dotenv file");

    assert!(environment.get("OPENROUTER_API_KEY").is_none());
}

#[test]
fn dotenv_debug_reports_only_the_redacted_entry_count() {
    let file = dotenv_file("OPENROUTER_API_KEY=hunter2\nOCTOROUTE_API_KEY=private\n");
    let environment = DotenvEnvironment::from_path(file.path(), TestEnvironment::default())
        .expect("valid dotenv file");

    assert_eq!(
        format!("{environment:?}"),
        "DotenvEnvironment { file_values: [REDACTED; 2], .. }"
    );
}
