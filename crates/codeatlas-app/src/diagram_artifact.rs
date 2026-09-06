use std::{
    ffi::OsStr,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use codeatlas_core::{AgentAnswer, AnswerId, Diagram, DiagramArtifact, DiagramDecision, DiagramId};
use codeatlas_diagram::{DiagramRenderError, render_svg};
use thiserror::Error;

const RENDERER_VERSION: &str = "svg-v1";
const MEDIA_TYPE: &str = "image/svg+xml";
const TEMPORARY_ATTEMPTS: usize = 128;
static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static STORE_LOCK: Mutex<()> = Mutex::new(());

pub(crate) fn validate_storage_root(
    configured_data_directory: &Path,
    repository_root: &Path,
) -> Result<(), DiagramGenerationError> {
    let data_directory = canonical_directory(configured_data_directory, "resolve data directory")?;
    let repository_root = canonical_directory(repository_root, "resolve repository root")?;
    if data_directory.starts_with(repository_root) {
        return Err(DiagramGenerationError::DataDirectoryInRepository);
    }
    Ok(())
}

pub(crate) fn render_and_store(
    data_directory: &Path,
    repository_root: &Path,
    answer: &mut AgentAnswer,
    cancellation: &dyn Fn() -> bool,
) -> Result<(), DiagramGenerationError> {
    ensure_active(cancellation)?;
    let answer_id = answer.id;
    let DiagramDecision::Needed { diagram, .. } = &mut answer.diagram else {
        return Ok(());
    };

    let mut identity_diagram = diagram.clone();
    identity_diagram.artifact = None;
    let canonical_json =
        serde_json::to_string(&identity_diagram).map_err(DiagramGenerationError::Serialize)?;
    let answer_id_text = answer_id.to_string();
    let diagram_id = DiagramId::from_stable_parts(&[
        answer_id_text.as_str(),
        RENDERER_VERSION,
        canonical_json.as_str(),
    ]);
    let svg = render_svg(&identity_diagram).map_err(DiagramGenerationError::Render)?;
    ensure_active(cancellation)?;
    let artifact = store_svg(
        data_directory,
        repository_root,
        answer_id,
        diagram_id,
        &svg,
        cancellation,
    )?;
    diagram.artifact = Some(artifact);
    Ok(())
}

fn ensure_active(cancellation: &dyn Fn() -> bool) -> Result<(), DiagramGenerationError> {
    if cancellation() {
        Err(DiagramGenerationError::Cancelled)
    } else {
        Ok(())
    }
}

pub(crate) fn open_in_default_viewer(
    configured_data_directory: &Path,
    diagram: &Diagram,
) -> Result<(), DiagramOpenError> {
    let artifact = diagram
        .artifact
        .as_ref()
        .ok_or(DiagramOpenError::MissingArtifact)?;
    let path = validate_openable_artifact(configured_data_directory, artifact)?;
    let mut identity_diagram = diagram.clone();
    identity_diagram.artifact = None;
    let expected = render_svg(&identity_diagram).map_err(DiagramOpenError::Render)?;
    validate_artifact_content(&path, &expected)?;
    launch_default_viewer(&path)
}

fn validate_artifact_content(path: &Path, expected: &[u8]) -> Result<(), DiagramOpenError> {
    let actual = fs::read(path).map_err(|source| DiagramOpenError::Io {
        operation: "read diagram artifact before opening",
        source,
    })?;
    if actual == expected {
        Ok(())
    } else {
        Err(DiagramOpenError::ArtifactContentMismatch)
    }
}

fn validate_openable_artifact(
    configured_data_directory: &Path,
    artifact: &DiagramArtifact,
) -> Result<PathBuf, DiagramOpenError> {
    if artifact.media_type != MEDIA_TYPE {
        return Err(DiagramOpenError::UnsupportedMediaType);
    }
    let data_directory = canonical_directory(configured_data_directory, "resolve data directory")
        .map_err(|_| DiagramOpenError::UnsafeArtifactPath)?;
    let diagrams_directory = data_directory.join("diagrams");
    validate_managed_directory(&diagrams_directory, &data_directory)
        .map_err(|_| DiagramOpenError::UnsafeArtifactPath)?;
    let diagram_root = diagrams_directory.join("v1");
    validate_managed_directory(&diagram_root, &diagrams_directory)
        .map_err(|_| DiagramOpenError::UnsafeArtifactPath)?;

    let target = PathBuf::from(&artifact.path);
    if !target.is_absolute()
        || target.file_name() != Some(OsStr::new(&format!("{}.svg", artifact.id)))
    {
        return Err(DiagramOpenError::UnsafeArtifactPath);
    }
    let Some(answer_directory) = target.parent() else {
        return Err(DiagramOpenError::UnsafeArtifactPath);
    };
    validate_managed_directory(answer_directory, &diagram_root)
        .map_err(|_| DiagramOpenError::UnsafeArtifactPath)?;
    let metadata = fs::symlink_metadata(&target).map_err(|source| DiagramOpenError::Io {
        operation: "inspect diagram artifact",
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DiagramOpenError::UnsafeArtifactPath);
    }
    let canonical = fs::canonicalize(&target).map_err(|source| DiagramOpenError::Io {
        operation: "resolve diagram artifact",
        source,
    })?;
    if canonical != target
        || canonical.parent() != Some(answer_directory)
        || !canonical.starts_with(&diagram_root)
    {
        return Err(DiagramOpenError::UnsafeArtifactPath);
    }
    if metadata.len() != artifact.byte_size {
        return Err(DiagramOpenError::ArtifactSizeMismatch);
    }
    Ok(canonical)
}

#[cfg(target_os = "windows")]
fn launch_default_viewer(path: &Path) -> Result<(), DiagramOpenError> {
    run_viewer("explorer.exe", path.as_os_str())
}

#[cfg(target_os = "macos")]
fn launch_default_viewer(path: &Path) -> Result<(), DiagramOpenError> {
    run_viewer("open", path.as_os_str())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn launch_default_viewer(path: &Path) -> Result<(), DiagramOpenError> {
    if is_wsl() {
        let output = Command::new("wslpath")
            .arg("-w")
            .arg(path)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .map_err(|source| DiagramOpenError::Io {
                operation: "convert WSL diagram path",
                source,
            })?;
        if !output.status.success() {
            return Err(DiagramOpenError::PathConversionFailed);
        }
        let windows_path =
            String::from_utf8(output.stdout).map_err(|_| DiagramOpenError::PathConversionFailed)?;
        let windows_path = windows_path.trim();
        if windows_path.is_empty() {
            return Err(DiagramOpenError::PathConversionFailed);
        }
        return run_viewer("explorer.exe", OsStr::new(windows_path));
    }
    run_viewer("xdg-open", path.as_os_str())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn is_wsl() -> bool {
    std::env::var_os("WSL_INTEROP").is_some()
        || std::env::var_os("WSL_DISTRO_NAME").is_some()
        || fs::read_to_string("/proc/sys/kernel/osrelease")
            .is_ok_and(|release| release.to_ascii_lowercase().contains("microsoft"))
}

#[cfg(not(any(unix, target_os = "windows")))]
fn launch_default_viewer(_path: &Path) -> Result<(), DiagramOpenError> {
    Err(DiagramOpenError::UnsupportedPlatform)
}

fn run_viewer(program: &'static str, path: &OsStr) -> Result<(), DiagramOpenError> {
    let status = Command::new(program)
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|source| DiagramOpenError::ViewerLaunch { program, source })?;
    if status.success() {
        Ok(())
    } else {
        Err(DiagramOpenError::ViewerFailed { program })
    }
}

fn store_svg(
    configured_data_directory: &Path,
    repository_root: &Path,
    answer_id: AnswerId,
    diagram_id: DiagramId,
    svg: &[u8],
    cancellation: &dyn Fn() -> bool,
) -> Result<DiagramArtifact, DiagramGenerationError> {
    let data_directory = canonical_directory(configured_data_directory, "resolve data directory")?;
    let repository_root = canonical_directory(repository_root, "resolve repository root")?;
    if data_directory.starts_with(&repository_root) {
        return Err(DiagramGenerationError::DataDirectoryInRepository);
    }
    ensure_active(cancellation)?;

    let diagrams_directory = prepare_managed_directory(
        &data_directory.join("diagrams"),
        &data_directory,
        &data_directory,
        &repository_root,
    )?;
    let diagram_root = prepare_managed_directory(
        &diagrams_directory.join("v1"),
        &diagrams_directory,
        &data_directory,
        &repository_root,
    )?;
    let answer_directory = prepare_managed_directory(
        &diagram_root.join(answer_id.to_string()),
        &diagram_root,
        &data_directory,
        &repository_root,
    )?;
    let target = answer_directory.join(format!("{diagram_id}.svg"));

    let _store_guard = STORE_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    ensure_active(cancellation)?;
    let canonical_target = match existing_target(&target, &answer_directory, svg)? {
        Some(path) => path,
        None => persist_new_target(
            &target,
            &answer_directory,
            &diagram_root,
            answer_id,
            diagram_id,
            svg,
        )?,
    };
    restrict_file_permissions(&canonical_target);

    let path = canonical_target
        .to_str()
        .ok_or(DiagramGenerationError::NonUtf8ArtifactPath)?
        .to_owned();
    let byte_size =
        u64::try_from(svg.len()).map_err(|_| DiagramGenerationError::ArtifactSizeOverflow)?;
    Ok(DiagramArtifact {
        id: diagram_id,
        path,
        media_type: MEDIA_TYPE.to_owned(),
        byte_size,
    })
}

fn canonical_directory(
    path: &Path,
    operation: &'static str,
) -> Result<PathBuf, DiagramGenerationError> {
    let canonical = fs::canonicalize(path)
        .map_err(|source| DiagramGenerationError::Io { operation, source })?;
    let metadata = fs::metadata(&canonical)
        .map_err(|source| DiagramGenerationError::Io { operation, source })?;
    if !metadata.is_dir() {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    Ok(canonical)
}

fn prepare_managed_directory(
    path: &Path,
    expected_parent: &Path,
    data_directory: &Path,
    repository_root: &Path,
) -> Result<PathBuf, DiagramGenerationError> {
    if path.parent() != Some(expected_parent) {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    reject_unsafe_existing_directory(path)?;
    create_private_dir_all(path).map_err(|source| DiagramGenerationError::Io {
        operation: "create diagram directory",
        source,
    })?;
    reject_unsafe_existing_directory(path)?;

    let canonical = fs::canonicalize(path).map_err(|source| DiagramGenerationError::Io {
        operation: "resolve diagram directory",
        source,
    })?;
    if canonical != path
        || canonical.parent() != Some(expected_parent)
        || !canonical.starts_with(data_directory)
        || canonical.starts_with(repository_root)
    {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    restrict_directory_permissions(&canonical);
    Ok(canonical)
}

fn reject_unsafe_existing_directory(path: &Path) -> Result<(), DiagramGenerationError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            Err(DiagramGenerationError::UnsafeStoragePath)
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(DiagramGenerationError::Io {
            operation: "inspect diagram directory",
            source,
        }),
    }
}

fn persist_new_target(
    target: &Path,
    answer_directory: &Path,
    diagram_root: &Path,
    answer_id: AnswerId,
    diagram_id: DiagramId,
    svg: &[u8],
) -> Result<PathBuf, DiagramGenerationError> {
    let temporary = write_temporary_file(answer_directory, answer_id, diagram_id, svg)?;
    let result = (|| {
        validate_managed_directory(answer_directory, diagram_root)?;
        if let Some(path) = existing_target(target, answer_directory, svg)? {
            return Ok(path);
        }

        if let Err(source) = fs::rename(&temporary, target) {
            if let Some(path) = existing_target(target, answer_directory, svg)? {
                return Ok(path);
            }
            return Err(DiagramGenerationError::Io {
                operation: "commit diagram artifact",
                source,
            });
        }
        sync_directory(answer_directory).map_err(|source| DiagramGenerationError::Io {
            operation: "sync diagram directory",
            source,
        })?;
        existing_target(target, answer_directory, svg)?
            .ok_or(DiagramGenerationError::MissingCommittedArtifact)
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn write_temporary_file(
    directory: &Path,
    answer_id: AnswerId,
    diagram_id: DiagramId,
    bytes: &[u8],
) -> Result<PathBuf, DiagramGenerationError> {
    let answer_id_text = answer_id.to_string();
    let diagram_id_text = diagram_id.to_string();
    let process_id = std::process::id().to_string();
    for _ in 0..TEMPORARY_ATTEMPTS {
        let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let sequence = sequence.to_string();
        let temporary_id = DiagramId::from_stable_parts(&[
            answer_id_text.as_str(),
            diagram_id_text.as_str(),
            "temporary",
            process_id.as_str(),
            sequence.as_str(),
        ]);
        let path = directory.join(format!(".tmp-{temporary_id}"));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = match options.open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(DiagramGenerationError::Io {
                    operation: "create temporary diagram artifact",
                    source,
                });
            }
        };
        if let Err(source) = file.write_all(bytes).and_then(|()| file.sync_all()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err(DiagramGenerationError::Io {
                operation: "write temporary diagram artifact",
                source,
            });
        }
        return Ok(path);
    }
    Err(DiagramGenerationError::TemporaryNameExhausted)
}

fn existing_target(
    target: &Path,
    expected_parent: &Path,
    expected_bytes: &[u8],
) -> Result<Option<PathBuf>, DiagramGenerationError> {
    let metadata = match fs::symlink_metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(DiagramGenerationError::Io {
                operation: "inspect diagram artifact",
                source,
            });
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }

    let canonical = fs::canonicalize(target).map_err(|source| DiagramGenerationError::Io {
        operation: "resolve diagram artifact",
        source,
    })?;
    if canonical != target || canonical.parent() != Some(expected_parent) {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    let actual = fs::read(&canonical).map_err(|source| DiagramGenerationError::Io {
        operation: "read diagram artifact",
        source,
    })?;
    if actual != expected_bytes {
        return Err(DiagramGenerationError::ArtifactConflict);
    }
    Ok(Some(canonical))
}

fn validate_managed_directory(
    path: &Path,
    expected_parent: &Path,
) -> Result<(), DiagramGenerationError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| DiagramGenerationError::Io {
        operation: "inspect diagram directory",
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    let canonical = fs::canonicalize(path).map_err(|source| DiagramGenerationError::Io {
        operation: "resolve diagram directory",
        source,
    })?;
    if canonical != path || canonical.parent() != Some(expected_parent) {
        return Err(DiagramGenerationError::UnsafeStoragePath);
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    let mut builder = fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder.create(path)
}

#[cfg(not(unix))]
fn create_private_dir_all(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

#[cfg(unix)]
fn restrict_directory_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_directory_permissions(_path: &Path) {}

#[cfg(unix)]
fn restrict_file_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_file_permissions(_path: &Path) {}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> io::Result<()> {
    Ok(())
}

#[derive(Debug, Error)]
pub(crate) enum DiagramGenerationError {
    #[error("diagram generation was cancelled")]
    Cancelled,
    #[error("diagram identity serialization failed")]
    Serialize(#[source] serde_json::Error),
    #[error("diagram rendering failed")]
    Render(#[source] DiagramRenderError),
    #[error("the data directory is inside the analyzed repository")]
    DataDirectoryInRepository,
    #[error("diagram storage path failed a safety check")]
    UnsafeStoragePath,
    #[error("diagram artifact conflicts with existing content")]
    ArtifactConflict,
    #[error("diagram artifact disappeared after commit")]
    MissingCommittedArtifact,
    #[error("diagram artifact path is not valid UTF-8")]
    NonUtf8ArtifactPath,
    #[error("diagram artifact size cannot be represented")]
    ArtifactSizeOverflow,
    #[error("could not allocate a temporary diagram artifact name")]
    TemporaryNameExhausted,
    #[error("diagram storage failed during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[derive(Debug, Error)]
pub(crate) enum DiagramOpenError {
    #[error("the diagram has no generated SVG artifact")]
    MissingArtifact,
    #[error("diagram artifact media type is not image/svg+xml")]
    UnsupportedMediaType,
    #[error("diagram artifact path failed a safety check")]
    UnsafeArtifactPath,
    #[error("diagram artifact size no longer matches the saved answer")]
    ArtifactSizeMismatch,
    #[error("diagram artifact content no longer matches its deterministic rendering")]
    ArtifactContentMismatch,
    #[error("diagram could not be reproduced before opening")]
    Render(#[source] DiagramRenderError),
    #[error("could not convert the diagram path for Windows")]
    PathConversionFailed,
    #[cfg(not(any(unix, target_os = "windows")))]
    #[error("this platform has no supported default diagram viewer")]
    UnsupportedPlatform,
    #[error("could not launch diagram viewer {program}")]
    ViewerLaunch {
        program: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("diagram viewer {program} exited unsuccessfully")]
    ViewerFailed { program: &'static str },
    #[error("diagram opening failed during {operation}")]
    Io {
        operation: &'static str,
        #[source]
        source: io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn openable_artifact() -> (TempDir, PathBuf, DiagramArtifact) {
        let temporary = TempDir::new().expect("temporary directory");
        let data_directory = temporary.path().join("data");
        let answer_id = AnswerId::from_stable_parts(&["open", "answer"]);
        let diagram_id = DiagramId::from_stable_parts(&["open", "diagram"]);
        let answer_directory = data_directory
            .join("diagrams")
            .join("v1")
            .join(answer_id.to_string());
        fs::create_dir_all(&answer_directory).expect("managed diagram directories");
        let target = answer_directory.join(format!("{diagram_id}.svg"));
        let bytes = b"<svg/>";
        fs::write(&target, bytes).expect("diagram fixture");
        let artifact = DiagramArtifact {
            id: diagram_id,
            path: target.to_string_lossy().into_owned(),
            media_type: MEDIA_TYPE.to_owned(),
            byte_size: u64::try_from(bytes.len()).expect("fixture size"),
        };
        (temporary, data_directory, artifact)
    }

    #[test]
    fn managed_svg_artifact_is_openable() {
        let (_temporary, data_directory, artifact) = openable_artifact();

        let path = validate_openable_artifact(&data_directory, &artifact)
            .expect("managed SVG should pass validation");

        assert_eq!(path, PathBuf::from(&artifact.path));
        validate_artifact_content(&path, b"<svg/>")
            .expect("unchanged deterministic SVG should pass validation");
    }

    #[test]
    fn artifact_opening_rejects_metadata_and_path_tampering() {
        let (_temporary, data_directory, artifact) = openable_artifact();

        let mut wrong_media_type = artifact.clone();
        wrong_media_type.media_type = "text/html".to_owned();
        assert!(matches!(
            validate_openable_artifact(&data_directory, &wrong_media_type),
            Err(DiagramOpenError::UnsupportedMediaType)
        ));

        let mut wrong_size = artifact.clone();
        wrong_size.byte_size = wrong_size.byte_size.saturating_add(1);
        assert!(matches!(
            validate_openable_artifact(&data_directory, &wrong_size),
            Err(DiagramOpenError::ArtifactSizeMismatch)
        ));

        let mut outside = artifact.clone();
        outside.path = "/tmp/unmanaged-codeatlas-diagram.svg".to_owned();
        assert!(matches!(
            validate_openable_artifact(&data_directory, &outside),
            Err(DiagramOpenError::UnsafeArtifactPath)
        ));

        fs::write(&artifact.path, b"<xvg/>").expect("same-sized tampered SVG fixture");
        assert!(matches!(
            validate_artifact_content(Path::new(&artifact.path), b"<svg/>"),
            Err(DiagramOpenError::ArtifactContentMismatch)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_opening_rejects_a_symlinked_svg() {
        use std::os::unix::fs::symlink;

        let (temporary, data_directory, artifact) = openable_artifact();
        let target = PathBuf::from(&artifact.path);
        let outside = temporary.path().join("outside.svg");
        fs::write(&outside, b"<svg/>").expect("outside fixture");
        fs::remove_file(&target).expect("replace managed file");
        symlink(outside, target).expect("symlink fixture");

        assert!(matches!(
            validate_openable_artifact(&data_directory, &artifact),
            Err(DiagramOpenError::UnsafeArtifactPath)
        ));
    }
}
