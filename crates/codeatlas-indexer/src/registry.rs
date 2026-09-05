use std::{
    any::Any,
    collections::HashMap,
    fmt,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::Arc,
};

use codeatlas_core::{
    Language, ParseInput, ParsedFile, ParserAdapter, ParserError, RepositoryPath,
};
use thiserror::Error;

struct RegisteredParser {
    adapter: Arc<dyn ParserAdapter>,
    revision: String,
}

#[derive(Default)]
pub struct ParserRegistry {
    parsers: HashMap<Language, RegisteredParser>,
}

impl ParserRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers an adapter using the current package version as its cache revision.
    pub fn register(&mut self, adapter: Arc<dyn ParserAdapter>) {
        self.register_versioned(adapter, env!("CARGO_PKG_VERSION"));
    }

    /// Registers an adapter and an explicit revision used for cache invalidation.
    pub fn register_versioned(
        &mut self,
        adapter: Arc<dyn ParserAdapter>,
        revision: impl Into<String>,
    ) {
        self.parsers.insert(
            adapter.language(),
            RegisteredParser {
                adapter,
                revision: revision.into(),
            },
        );
    }

    #[must_use]
    pub fn contains(&self, language: &Language) -> bool {
        self.parsers.contains_key(language)
    }

    /// Dispatches one file and contains adapter panics at the file boundary.
    ///
    /// # Errors
    ///
    /// Returns [`RegistryError`] for an unsupported language, parser failure,
    /// adapter panic, or malformed adapter output.
    pub fn parse(
        &self,
        language: &Language,
        input: ParseInput<'_>,
    ) -> Result<ParsedFile, RegistryError> {
        let parser =
            self.parsers
                .get(language)
                .ok_or_else(|| RegistryError::UnsupportedLanguage {
                    path: input.path.clone(),
                    language: language.clone(),
                })?;
        let parsed = catch_unwind(AssertUnwindSafe(|| parser.adapter.parse(input)))
            .map_err(|panic| RegistryError::Panicked {
                path: input.path.clone(),
                message: panic_message(&*panic),
            })?
            .map_err(RegistryError::Parser)?;
        if parsed.path != *input.path {
            return Err(RegistryError::PathMismatch {
                expected: input.path.clone(),
                actual: parsed.path,
            });
        }
        if parsed.language != *language {
            return Err(RegistryError::LanguageMismatch {
                path: input.path.clone(),
                expected: language.clone(),
                actual: parsed.language,
            });
        }
        Ok(parsed)
    }

    pub(crate) fn revisions(&self) -> Vec<(String, String)> {
        let mut revisions = self
            .parsers
            .iter()
            .map(|(language, parser)| (language_key(language), parser.revision.clone()))
            .collect::<Vec<_>>();
        revisions.sort();
        revisions
    }
}

impl fmt::Debug for ParserRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ParserRegistry")
            .field("revisions", &self.revisions())
            .finish()
    }
}

#[derive(Debug, Error)]
pub enum RegistryError {
    #[error("no parser is registered for {language:?} ({path})")]
    UnsupportedLanguage {
        path: RepositoryPath,
        language: Language,
    },
    #[error(transparent)]
    Parser(ParserError),
    #[error("parser panicked while parsing {path}: {message}")]
    Panicked {
        path: RepositoryPath,
        message: String,
    },
    #[error("parser returned path {actual}, expected {expected}")]
    PathMismatch {
        expected: RepositoryPath,
        actual: RepositoryPath,
    },
    #[error("parser returned language {actual:?} for {path}, expected {expected:?}")]
    LanguageMismatch {
        path: RepositoryPath,
        expected: Language,
        actual: Language,
    },
}

impl RegistryError {
    #[must_use]
    pub fn path(&self) -> &RepositoryPath {
        match self {
            Self::UnsupportedLanguage { path, .. }
            | Self::Panicked { path, .. }
            | Self::LanguageMismatch { path, .. } => path,
            Self::Parser(error) => &error.path,
            Self::PathMismatch { expected, .. } => expected,
        }
    }

    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedLanguage { .. } => "parser_not_registered",
            Self::Parser(_) => "parse_error",
            Self::Panicked { .. } => "parser_panic",
            Self::PathMismatch { .. } => "parser_path_mismatch",
            Self::LanguageMismatch { .. } => "parser_language_mismatch",
        }
    }
}

fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(message) = panic.downcast_ref::<&str>() {
        (*message).to_owned()
    } else if let Some(message) = panic.downcast_ref::<String>() {
        message.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

fn language_key(language: &Language) -> String {
    match language {
        Language::Rust => "rust".to_owned(),
        Language::Python => "python".to_owned(),
        Language::C => "c".to_owned(),
        Language::Cpp => "cpp".to_owned(),
        Language::Java => "java".to_owned(),
        Language::JavaScript => "javascript".to_owned(),
        Language::TypeScript => "typescript".to_owned(),
        Language::Go => "go".to_owned(),
        Language::Other(value) => format!("other:{value}"),
    }
}
