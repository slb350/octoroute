//! Secret-bearing process and repository `.env` configuration.

use secrecy::SecretString;
use std::{
    collections::HashMap,
    fmt,
    path::{Path, PathBuf},
};
use thiserror::Error;

/// Source used to resolve secret-bearing environment variables.
pub trait Environment {
    /// Return an environment variable without logging its value.
    ///
    /// The value is a [`SecretString`] because every caller of this trait is
    /// resolving a credential. A plain `String` puts the secret one `Debug`
    /// format or stray interpolation away from a log line.
    fn get(&self, name: &str) -> Option<SecretString>;
}

/// Environment source backed by the current process.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProcessEnvironment;

impl Environment for ProcessEnvironment {
    fn get(&self, name: &str) -> Option<SecretString> {
        std::env::var(name).ok().map(SecretString::from)
    }
}

/// Environment composed from process values over an optional dotenv file.
pub struct DotenvEnvironment<E> {
    parent: E,
    file_values: HashMap<String, SecretString>,
}

impl<E> DotenvEnvironment<E> {
    /// Load a required dotenv file without mutating process-global state.
    pub fn from_path(path: &Path, parent: E) -> Result<Self, DotenvLoadError> {
        let file_values = load_values(path)?;
        Ok(Self {
            parent,
            file_values,
        })
    }

    /// Load a dotenv file if present, otherwise use the parent alone.
    pub fn from_optional_path(path: &Path, parent: E) -> Result<Self, DotenvLoadError> {
        let file_values = match load_values(path) {
            Ok(values) => values,
            Err(DotenvLoadError::NotFound { .. }) => HashMap::new(),
            Err(error) => return Err(error),
        };
        Ok(Self {
            parent,
            file_values,
        })
    }
}

impl<E: Environment> Environment for DotenvEnvironment<E> {
    fn get(&self, name: &str) -> Option<SecretString> {
        self.parent
            .get(name)
            .or_else(|| self.file_values.get(name).cloned())
    }
}

impl<E> fmt::Debug for DotenvEnvironment<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DotenvEnvironment")
            .field(
                "file_values",
                &format_args!("[REDACTED; {}]", self.file_values.len()),
            )
            .finish_non_exhaustive()
    }
}

/// Dotenv errors which never include file contents.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum DotenvLoadError {
    /// The optional file was not present.
    #[error("dotenv file `{path}` was not found")]
    NotFound {
        /// Missing path.
        path: PathBuf,
    },
    /// The file could not be read.
    #[error("could not read dotenv file `{path}`")]
    Io {
        /// Unreadable path.
        path: PathBuf,
    },
    /// The file contained invalid dotenv syntax.
    #[error("dotenv file `{path}` is invalid; contents omitted")]
    Invalid {
        /// Invalid path.
        path: PathBuf,
    },
}

fn load_error(path: &Path, error: dotenvy::Error, parsing: bool) -> DotenvLoadError {
    if error.not_found() {
        DotenvLoadError::NotFound {
            path: path.to_path_buf(),
        }
    } else if parsing || matches!(error, dotenvy::Error::LineParse(..)) {
        DotenvLoadError::Invalid {
            path: path.to_path_buf(),
        }
    } else {
        DotenvLoadError::Io {
            path: path.to_path_buf(),
        }
    }
}

fn load_values(path: &Path) -> Result<HashMap<String, SecretString>, DotenvLoadError> {
    let iterator = dotenvy::from_path_iter(path).map_err(|error| load_error(path, error, false))?;
    let mut file_values = HashMap::new();
    for item in iterator {
        let (name, value) = item.map_err(|error| load_error(path, error, true))?;
        file_values
            .entry(name)
            .or_insert_with(|| SecretString::from(value));
    }
    Ok(file_values)
}
