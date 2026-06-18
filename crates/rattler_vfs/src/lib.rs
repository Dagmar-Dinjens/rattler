use crate::metadata::{CustomPrefixPlaceholder, FSMetadata};
use anyhow::anyhow;
use rattler_cache::package_cache::CacheKey;
use rattler_conda_types::{
    Platform,
    package::{FileMode, PathsJson},
};
use rattler_lock::{LockFile, LockedPackage};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

pub mod backends;
pub mod metadata;
pub mod mount;
pub mod prefix_placeholder;
pub mod prefix_replacement;
pub mod virtual_fs_core;

pub mod tests;

pub use mount::{MountBackend, MountSession};

/// Wraps an inner [`MountSession`] and keeps a [`tempfile::TempDir`] alive
/// alongside it. The temp dir is dropped *after* unmount completes so that
/// files materialized there are still accessible during the unmount I/O.
struct BoundSession {
    inner: Option<Box<dyn MountSession>>,
    _temp_dir: Option<tempfile::TempDir>,
}

impl MountSession for BoundSession {
    fn unmount(mut self: Box<Self>) -> anyhow::Result<()> {
        let inner = self.inner.take().unwrap();
        let temp_dir = self._temp_dir.take();
        let result = inner.unmount();
        drop(temp_dir);
        result
    }
}

pub async fn mount_environment(
    pixi_lock: PathBuf,
    cache_origin: PathBuf,
    mount_dir: PathBuf,
    backend: MountBackend,
    environment_name: String,
) -> anyhow::Result<Box<dyn MountSession>> {
    let package_refs = solve_environment(&pixi_lock, &environment_name)?;

    // On macOS, binary files with prefix replacement are materialized into this
    // temp dir and re-signed so the kernel will execute them.
    #[cfg(target_os = "macos")]
    let temp_dir = tempfile::TempDir::new()?;

    let mut metadata = vec![FSMetadata::new_directory(PathBuf::from("."), 0)];
    let mut directory_indices = HashMap::new();
    directory_indices.insert(PathBuf::from("."), 0);

    for package_ref in package_refs {
        let (paths_json, package_dir) = get_paths_json(&package_ref, &cache_origin)?;
        path_parse(
            paths_json,
            package_dir,
            &mut metadata,
            &mut directory_indices,
            &mount_dir,
            #[cfg(target_os = "macos")]
            Some(temp_dir.path()),
            #[cfg(not(target_os = "macos"))]
            None,
        )?;
    }

    let session = backends::generate_mount(backend, metadata, mount_dir).await?;

    Ok(Box::new(BoundSession {
        inner: Some(session),
        #[cfg(target_os = "macos")]
        _temp_dir: Some(temp_dir),
        #[cfg(not(target_os = "macos"))]
        _temp_dir: None,
    }))
}

pub fn solve_environment(
    pixi_lock: &Path,
    environment_name: &str,
) -> anyhow::Result<Vec<LockedPackage>> {
    let lockfile = LockFile::from_path(pixi_lock)?;

    let env = lockfile
        .environment(environment_name)
        .ok_or_else(|| anyhow!("environment not found"))?;

    let platform_name = Platform::current().to_string();
    let platform = lockfile
        .platform(&platform_name)
        .ok_or_else(|| anyhow!("lockfile does not contain platform {platform_name}"))?;

    let packages = env
        .packages(platform)
        .ok_or_else(|| anyhow!("environment does not contain packages for current platform"))?;

    Ok(packages.cloned().collect())
}

pub fn get_paths_json(
    package_ref: &LockedPackage,
    cache_origin: &Path,
) -> anyhow::Result<(PathsJson, PathBuf)> {
    let package_data = package_ref
        .as_binary_conda()
        .ok_or_else(|| anyhow!("only binary conda packages can be mounted"))?;
    let cache_key = CacheKey::from(&package_data.package_record);
    let cache_path = cache_origin.join(cache_key.to_string());
    let paths_json = PathsJson::from_package_directory_with_deprecated_fallback(&cache_path)?;

    Ok((paths_json, cache_path))
}

pub fn path_parse(
    paths_json: PathsJson,
    package_dir: PathBuf,
    env_paths: &mut Vec<FSMetadata>,
    directory_indices: &mut HashMap<PathBuf, usize>,
    mount_point: &Path,
    materialize_dir: Option<&Path>,
) -> anyhow::Result<()> {
    for path in &paths_json.paths {
        let cache_base: Arc<Path> = package_dir.clone().into();
        let parent_directory = path.relative_path.parent().unwrap_or(Path::new("."));

        let mut parent_index = 0;
        for component in parent_directory.components() {
            let current_path = env_paths[parent_index]
                .as_directory()
                .expect("first element is always the root directory")
                .prefix_path
                .join(component);

            parent_index = match directory_indices.get(&current_path) {
                Some(&index) => index,
                None => {
                    let new_dir = FSMetadata::new_directory(current_path.clone(), parent_index);
                    let child_index = env_paths.len();
                    env_paths.push(new_dir);
                    env_paths[parent_index]
                        .as_directory_mut()
                        .expect("parent is a directory")
                        .children
                        .push(child_index);
                    directory_indices.insert(current_path, child_index);
                    child_index
                }
            };
        }

        let file_name = path
            .relative_path
            .file_name()
            .expect("files always have names");
        let file_path = (*cache_base).join(&path.relative_path);

        // `effective_base` stays as `cache_base` unless we materialize the
        // file into `materialize_dir`, in which case it points there instead.
        let mut effective_base = cache_base.clone();
        let prefix_placeholder = match path.prefix_placeholder.clone() {
            None => None,
            Some(pp) => {
                let source_bytes = std::fs::read(&file_path).unwrap_or_default();
                let custom = CustomPrefixPlaceholder::from_placeholder(pp, &source_bytes);

                #[cfg(target_os = "macos")]
                if custom.file_mode == FileMode::Binary
                    && !custom.offsets.is_empty()
                    && let Some(mat_dir) = materialize_dir
                {
                    materialize_and_sign(
                        &custom,
                        &file_path,
                        mat_dir,
                        &path.relative_path,
                        mount_point,
                    )?;
                    effective_base = Arc::from(mat_dir);
                    None // NFS serves the pre-replaced file; no in-flight replacement needed
                } else {
                    Some(custom)
                }

                #[cfg(not(target_os = "macos"))]
                Some(custom)
            }
        };

        let file_index = env_paths.len();
        env_paths.push(FSMetadata::new_file(
            file_name.into(),
            parent_index,
            effective_base,
            path.path_type.clone(),
            prefix_placeholder,
        ));

        env_paths[parent_index]
            .as_directory_mut()
            .expect("parents are always directories")
            .children
            .push(file_index);
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn materialize_and_sign(
    placeholder: &CustomPrefixPlaceholder,
    original_path: &Path,
    mat_dir: &Path,
    rel_path: &Path,
    mount_point: &Path,
) -> anyhow::Result<()> {
    use crate::prefix_replacement::binary_prefix_replacement;
    use memmap2::Mmap;
    use std::fs::File;
    use std::os::unix::fs::PermissionsExt;

    let fd = File::open(original_path)
        .map_err(|e| anyhow!("opening {}: {e}", original_path.display()))?;
    let mmap = unsafe { Mmap::map(&fd)? };

    let replaced =
        binary_prefix_replacement(placeholder, 0, mmap.len(), mmap.len(), &mmap, mount_point);

    let dest_dir = mat_dir.join(rel_path.parent().unwrap_or(Path::new(".")));
    std::fs::create_dir_all(&dest_dir)?;
    let dest = dest_dir.join(rel_path.file_name().expect("files always have names"));
    std::fs::write(&dest, &replaced)?;

    let orig_mode = std::fs::metadata(original_path)?.permissions().mode();
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(orig_mode | 0o111))?;

    rattler::install::codesign(&dest)
        .map_err(|e| anyhow!("codesign failed for {}: {e:?}", dest.display()))?;

    Ok(())
}
