//! Safe repository scanning and language-neutral index assembly.

mod assembler;
mod diagnostic;
mod indexer;
mod registry;
mod scanner;
mod store;

pub use assembler::{AssemblyError, IndexAssembler, RepositorySpec};
pub use diagnostic::{DiagnosticSeverity, DiagnosticStage, FileDiagnostic};
pub use indexer::{IndexCounts, IndexError, IndexReport, Indexer, ProgressCallback};
pub use registry::{ParserRegistry, RegistryError};
pub use scanner::{
    CancellationCheck, RepositoryScanner, ScanConfig, ScanError, ScanResult, ScannedFile,
    language_for_path,
};
pub use store::{CachedIndex, IndexStoreError, JsonIndexStore};
