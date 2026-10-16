//! Shared fixture for the `rattler_vfs` benchmarks that need a real
//! environment.
//!
//! The environment comes from `test-data/rattler-vfs/pixi.lock` (python +
//! numpy + pip + pytest; the same fixture the e2e mount test uses). Packages
//! are fetched into the default rattler package cache on first use, so the
//! first run needs network access and later runs measure a warm cache.
//!
//! Environment variables:
//! - `RATTLER_VFS_BENCH_LOCK`: use another lock file.
//! - `RATTLER_VFS_BENCH_ENV`: environment name inside the lock file
//!   (default `default`).
//! - `RATTLER_VFS_BENCH_NO_MOUNT=1`: skip benchmarks that need a kernel mount.

#![allow(dead_code)]

use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::OnceLock,
};

use rattler_cache::{default_cache_dir, package_cache::PackageCache};
use rattler_conda_types::{Platform, RepoDataRecord};
use rattler_lock::LockFile;
use rattler_vfs::{MetadataTree, Mode, MountConfig, Transport, compute_env_hash};

pub struct Fixture {
    pub lockfile: LockFile,
    pub environment: String,
    pub platform: Platform,
    pub package_cache: PackageCache,
    pub env_hash: String,
}

impl Fixture {
    pub fn records(&self) -> Vec<RepoDataRecord> {
        let env = self
            .lockfile
            .environment(&self.environment)
            .expect("environment in lock file");
        let platform = self
            .lockfile
            .platform(self.platform.as_str())
            .expect("platform in lock file");
        env.conda_repodata_records(platform)
            .expect("convert lock file records")
            .expect("records for platform")
    }

    pub async fn tree(&self, mount_point: &Path) -> MetadataTree {
        MetadataTree::build(
            &self.lockfile,
            &self.environment,
            self.platform,
            &self.package_cache,
            mount_point,
        )
        .await
        .expect("build metadata tree")
    }

    pub fn mount_config(&self, mount_point: &Path) -> MountConfig {
        // ProjFS has no read-only mode; fall back to writable there, exactly
        // like `rattler exec --dematerialized`.
        MountConfig::new(
            mount_point.to_path_buf(),
            Transport::best(),
            self.env_hash.clone(),
        )
        .with_mode(Mode::ReadOnlyIfSupported)
    }
}

pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

/// Load the fixture and make sure every package is in the package cache, so
/// no benchmark iteration pays for downloads.
pub fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let lock_path = std::env::var_os("RATTLER_VFS_BENCH_LOCK").map_or_else(
            || Path::new(env!("CARGO_MANIFEST_DIR")).join("../../test-data/rattler-vfs/pixi.lock"),
            PathBuf::from,
        );
        let environment =
            std::env::var("RATTLER_VFS_BENCH_ENV").unwrap_or_else(|_| "default".to_string());
        let lockfile = LockFile::from_path(&lock_path).expect("parse lock file");
        let platform = Platform::current().expect("host platform");
        let package_cache = PackageCache::new(
            default_cache_dir()
                .expect("cache dir")
                .join(rattler_cache::PACKAGE_CACHE_DIR),
        );
        let env_hash =
            compute_env_hash(&lockfile, &environment, platform).expect("compute env hash");
        let fixture = Fixture {
            lockfile,
            environment,
            platform,
            package_cache,
            env_hash,
        };
        // Building the tree once fetches anything missing from the cache.
        let scratch = tempfile::tempdir().expect("tempdir");
        let tree = runtime().block_on(fixture.tree(scratch.path()));
        eprintln!(
            "rattler_vfs bench fixture: {} ({} records, {} tree nodes, transport {})",
            lock_path.display(),
            fixture.records().len(),
            tree.len(),
            Transport::best().name(),
        );
        fixture
    })
}

pub fn mounts_enabled() -> bool {
    if std::env::var_os("RATTLER_VFS_BENCH_NO_MOUNT").is_some_and(|v| v != "0") {
        return false;
    }
    let transport = Transport::best();
    if !transport.is_available() {
        eprintln!(
            "skipping mount benchmarks: transport {} is not available",
            transport.name()
        );
        return false;
    }
    true
}

/// Python interpreter inside a conda prefix.
pub fn python_in(prefix: &Path) -> PathBuf {
    if cfg!(windows) {
        prefix.join("python.exe")
    } else {
        prefix.join("bin").join("python")
    }
}

/// Totals from walking a directory tree.
#[derive(Debug, Default, Clone, Copy)]
pub struct WalkStats {
    pub files: u64,
    pub dirs: u64,
    pub bytes: u64,
}

/// `lstat` every entry below `root` (what `ls -lR` / an import path scan does).
pub fn stat_tree(root: &Path) -> WalkStats {
    let mut stats = WalkStats::default();
    walk(root, &mut |path, meta| {
        if meta.is_dir() {
            stats.dirs += 1;
        } else {
            stats.files += 1;
            stats.bytes += meta.len();
        }
        let _ = path;
    });
    stats
}

/// Read every regular file below `root` to the end.
pub fn read_tree(root: &Path) -> WalkStats {
    let mut stats = WalkStats::default();
    let mut buf = vec![0u8; 1 << 20];
    walk(root, &mut |path, meta| {
        if meta.is_dir() {
            stats.dirs += 1;
            return;
        }
        if !meta.is_file() {
            return;
        }
        stats.files += 1;
        if let Ok(mut file) = fs::File::open(path) {
            while let Ok(n) = file.read(&mut buf) {
                if n == 0 {
                    break;
                }
                stats.bytes += n as u64;
            }
        }
    });
    stats
}

fn walk(dir: &Path, visit: &mut dyn FnMut(&Path, &fs::Metadata)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        visit(&path, &meta);
        if meta.is_dir() {
            walk(&path, visit);
        }
    }
}
