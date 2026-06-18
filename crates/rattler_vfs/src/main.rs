use anyhow::Result;
use clap::{Arg, Command};
use compio;
use rattler_cache::{PACKAGE_CACHE_DIR, default_cache_dir};
use std::{collections::HashMap, path::PathBuf};

use rattler_vfs::{
    backends::generate_mount,
    metadata::FSMetadata,
    mount::{MountBackend, MountSession},
    path_parse, solve_environment,
};

#[compio::main]
async fn main() -> Result<()> {
    let args = handle_input_arguments()?;

    println!("Mounting environment...");
    println!("  lock: {:?}", args.pixi_lock);
    println!("  cache: {:?}", args.cache_origin);
    println!("  mount: {:?}", args.mount_dir);
    println!("  backend: {:?}", args.mount_type);

    let session = mount_environment(
        args.pixi_lock,
        args.cache_origin,
        args.mount_dir.clone(),
        args.mount_type,
        args.environment_name,
    )
    .await?;

    println!("Environment mounted at {}", args.mount_dir.display());

    println!("Press Ctrl+C to unmount.");

    compio::signal::ctrl_c().await?;

    println!("Unmounting...");

    session.unmount()?;

    println!("Done.");

    Ok(())
}
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
        let (paths_json, package_dir) = rattler_vfs::get_paths_json(&package_ref, &cache_origin)?;

        // pixi_lock:    _which packages to make available/ check for availability_ empty? can't be empty (can't make an empty env)
        // cache_origin: _where the packages are cached locally/ read from_         empty? default to regular cache location
        // mounting_dir: _where the packages optically are for the system_          empty? default to where the package is?
        // mount_type:   _which type backend (NFS/FUSE/etc.) to use_                empty? default to NFS/ best optimised for system
        path_parse(
            paths_json,
            package_dir,
            &mut metadata,
            &mut directory_indices,
        );
    }

    generate_mount(backend, metadata, mount_dir).await
}

pub struct MountArgs {
    pub pixi_lock: PathBuf,
    pub cache_origin: PathBuf,
    pub mount_dir: PathBuf,
    pub mount_type: MountBackend,
    pub environment_name: String,
}

fn handle_input_arguments() -> anyhow::Result<MountArgs> {
    let matches = Command::new("mount")
        .arg(
            Arg::new("PIXI_LOCK")
                .long("PIXI_LOCK")
                .required(true)
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("CACHE_ORIGIN")
                .long("CACHE_ORIGIN")
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("MOUNT_DIR")
                .long("MOUNT_DIR")
                .value_parser(clap::value_parser!(PathBuf)),
        )
        .arg(
            Arg::new("mount-type")
                .long("mount-type")
                .default_value("nfs"),
        )
        .arg(
            Arg::new("environment")
                .long("environment")
                .default_value("default"),
        )
        .get_matches();

    let pixi_lock = matches.get_one::<PathBuf>("PIXI_LOCK").unwrap().clone();

    let cache_origin = matches
        .get_one::<PathBuf>("CACHE_ORIGIN")
        .cloned()
        .unwrap_or_else(|| {
            default_cache_dir()
                .map(|cache_dir| cache_dir.join(PACKAGE_CACHE_DIR))
                .unwrap_or_else(|_| PathBuf::from(PACKAGE_CACHE_DIR))
        });

    let mount_dir = matches
        .get_one::<PathBuf>("MOUNT_DIR")
        .cloned()
        .unwrap_or_else(|| std::env::current_dir().unwrap());

    let mount_type = MountBackend::from(
        matches
            .get_one::<String>("mount-type")
            .map(String::as_str)
            .unwrap_or("nfs"),
    );

    let environment_name = matches.get_one::<String>("environment").unwrap().clone();

    Ok(MountArgs {
        pixi_lock,
        cache_origin,
        mount_dir,
        mount_type,
        environment_name,
    })
}
