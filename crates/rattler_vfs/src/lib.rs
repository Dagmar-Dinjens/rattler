use crate::metadata::FSMetadata;
use anyhow::anyhow;
use rattler_cache::package_cache::CacheKey;
use rattler_conda_types::{
    Platform,
    package::{PathsJson, PrefixPlaceholder},
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

pub async fn mount_environment(
    pixi_lock: PathBuf,
    cache_origin: PathBuf,
    mount_dir: PathBuf,
    backend: MountBackend,
    environment_name: String,
) -> anyhow::Result<Box<dyn MountSession>> {
    let package_refs = solve_environment(&pixi_lock, &environment_name)?;

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
        );
    }

    backends::generate_mount(backend, metadata, mount_dir).await
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
) {
    paths_json.paths.iter().for_each(|path| {
        let cachepath: Arc<Path> = package_dir.clone().into();
        let parent_directory = path.relative_path.parent().unwrap_or(Path::new("."));

        // let parent_components = parent_directory.components();
        let mut parent_index = 0;

        parent_directory.components().for_each(|component| {
            let current_path = env_paths[parent_index]
                .as_directory()
                .expect("First element is always the root directory")
                .prefix_path
                .join(component);

            parent_index = match directory_indices.get(&current_path) {
                Some(&index) => index,
                None => {
                    let new_dir = FSMetadata::new_directory(current_path.clone(), parent_index);
                    let child_index = env_paths.len(); // TODO: Is there a better way of knowing the index of the new item?

                    env_paths.push(new_dir);
                    env_paths[parent_index]
                        .as_directory_mut()
                        .expect("Parent is a directory")
                        .children
                        .push(child_index);

                    directory_indices.insert(current_path, child_index);
                    child_index
                }
            };
        });

        let file_name = path
            .relative_path
            .file_name()
            .expect("Files always have names");

        // maybe hide as prefix function
        let file_path = cachepath.join(&path.relative_path);
        let prefix_placeholder =
            path.prefix_placeholder
                .clone()
                .map(|prefix_placeholder: PrefixPlaceholder| {
                    let source_bytes = std::fs::read(&file_path).unwrap_or_default();
                    crate::metadata::CustomPrefixPlaceholder::from_placeholder(
                        prefix_placeholder,
                        &source_bytes,
                    )
                });

        let file_index = env_paths.len();
        env_paths.push(FSMetadata::new_file(
            file_name.into(),
            parent_index,
            cachepath.clone(),
            path.path_type.clone(),
            prefix_placeholder,
        ));

        // TODO: Is there a better way of knowing the index of the new item?
        env_paths[parent_index]
            .as_directory_mut()
            .expect("Parents are always directories")
            .children
            .push(file_index);
    });
    // println!("add the env_paths: {:#?}", &env_paths);
}
