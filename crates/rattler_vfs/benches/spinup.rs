//! Spinning up an environment on a mount vs installing it to disk, and what
//! using it costs afterwards.
//!
//! Both paths start from the same warm package cache, so neither pays for
//! downloads:
//! - *installed*: `rattler::install::Installer` links every file into a fresh
//!   prefix (what `pixi exec` / `rattler exec` do).
//! - *mounted*: `rattler_vfs` builds the metadata tree and mounts it with the
//!   platform's default transport (what `rattler exec --dematerialized` does).
//!
//! `spinup/*` measures time until the prefix is usable (and, in the `*_python`
//! variants, until `python -c pass` has run in it). `prefix_io/*` measures
//! warm use of an already-present prefix through each path.
//!
//! On Windows (`ProjFS`) a file read once is hydrated to disk, so warm
//! `prefix_io/*/mounted` numbers there approach on-disk numbers.

mod common;

use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::{
        OnceLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};
use rattler::install::Installer;
use rattler_vfs::{MountHandle, build_and_mount};
use tempfile::TempDir;

fn run_python(prefix: &Path, code: &str) {
    let status = Command::new(common::python_in(prefix))
        .args(["-c", code])
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .status()
        .expect("spawn python");
    assert!(
        status.success(),
        "python -c {code:?} failed in {}",
        prefix.display()
    );
}

/// An empty directory at a stable path, recreated per iteration.
///
/// `rattler exec` / `pixi exec` always rebuild an environment at the same
/// path, and on macOS the kernel's code-signature cache is keyed on it, so a
/// fresh random path per iteration would overstate first-run costs.
fn slot(name: &str) -> PathBuf {
    static BASE: OnceLock<TempDir> = OnceLock::new();
    let path = BASE
        .get_or_init(|| tempfile::tempdir().expect("tempdir"))
        .path()
        .join(name);
    if path.exists() {
        std::fs::remove_dir_all(&path).expect("clear previous iteration");
    }
    std::fs::create_dir_all(&path).expect("create slot");
    path
}

fn install(prefix: &Path) {
    let fixture = common::fixture();
    common::runtime()
        .block_on(
            Installer::new()
                .with_package_cache(fixture.package_cache.clone())
                .with_target_platform(fixture.platform)
                .install(prefix, fixture.records()),
        )
        .expect("install environment");
}

/// A live mount; dropping it unmounts.
struct Mounted {
    handle: Option<MountHandle>,
    prefix: PathBuf,
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = common::runtime().block_on(handle.unmount());
        }
    }
}

/// Mount attempts that failed and were retried. Rapid remounts at one path
/// occasionally fail on macOS NFS (`mount_nfs: ... Connection refused`); that
/// is reported rather than silently absorbed.
static MOUNT_RETRIES: AtomicUsize = AtomicUsize::new(0);

fn mount(prefix: PathBuf) -> Mounted {
    let fixture = common::fixture();
    let mut attempt = 0;
    let handle = loop {
        match common::runtime().block_on(build_and_mount(
            &fixture.lockfile,
            &fixture.environment,
            fixture.platform,
            &fixture.package_cache,
            &fixture.mount_config(&prefix),
        )) {
            Ok(handle) => break handle,
            Err(err) if attempt < 3 => {
                attempt += 1;
                let n = MOUNT_RETRIES.fetch_add(1, Ordering::Relaxed) + 1;
                eprintln!("mount failed (retry #{n} overall): {err:#}");
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(err) => panic!("mount environment: {err:#}"),
        }
    };
    // A mount is only usable once the kernel answers for it.
    std::fs::metadata(common::python_in(&prefix)).expect("python visible in mount");
    Mounted {
        handle: Some(handle),
        prefix,
    }
}

fn bench_spinup(c: &mut Criterion) {
    common::fixture();
    let mounts = common::mounts_enabled();
    let mut group = c.benchmark_group("spinup");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(30));

    // `iter_batched` returns each iteration's prefix / mount out of the timed
    // region, so deleting the install and unmounting are not measured (except
    // in `mounted_and_unmounted`).
    group.bench_function("installed", |b| {
        b.iter_batched(
            || slot("installed"),
            |prefix| install(&prefix),
            BatchSize::PerIteration,
        );
    });
    group.bench_function("installed_python", |b| {
        b.iter_batched(
            || slot("installed_python"),
            |prefix| {
                install(&prefix);
                run_python(&prefix, "pass");
            },
            BatchSize::PerIteration,
        );
    });
    if mounts {
        group.bench_function("mounted", |b| {
            b.iter_batched(|| slot("mounted"), mount, BatchSize::PerIteration);
        });
        group.bench_function("mounted_python", |b| {
            b.iter_batched(
                || slot("mounted_python"),
                |prefix| {
                    let m = mount(prefix);
                    run_python(&m.prefix, "pass");
                    m
                },
                BatchSize::PerIteration,
            );
        });
        group.bench_function("mounted_and_unmounted", |b| {
            b.iter_batched(
                || slot("mounted_and_unmounted"),
                |prefix| drop(mount(prefix)),
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
    eprintln!(
        "spinup: {} mount retries",
        MOUNT_RETRIES.load(Ordering::Relaxed)
    );
}

fn bench_prefix_io(c: &mut Criterion) {
    common::fixture();
    let installed = slot("io_installed");
    install(&installed);
    let mounted = common::mounts_enabled().then(|| mount(slot("io_mounted")));

    let mut prefixes = vec![("installed", installed)];
    if let Some(m) = &mounted {
        prefixes.push(("mounted", m.prefix.clone()));
    }
    for (label, prefix) in &prefixes {
        let stats = common::read_tree(prefix);
        eprintln!(
            "prefix_io/{label}: {} files, {} dirs, {} MiB",
            stats.files,
            stats.dirs,
            stats.bytes >> 20
        );
    }

    let mut group = c.benchmark_group("prefix_io");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));
    for (label, prefix) in &prefixes {
        group.bench_with_input(BenchmarkId::new("stat_tree", label), prefix, |b, p| {
            b.iter(|| common::stat_tree(p));
        });
        group.bench_with_input(BenchmarkId::new("read_tree", label), prefix, |b, p| {
            b.iter(|| common::read_tree(p));
        });
        group.bench_with_input(BenchmarkId::new("python_startup", label), prefix, |b, p| {
            b.iter(|| run_python(p, "pass"));
        });
        group.bench_with_input(BenchmarkId::new("import_numpy", label), prefix, |b, p| {
            b.iter(|| run_python(p, "import numpy"));
        });
    }
    group.finish();
    drop(mounted);
}

criterion_group!(benches, bench_spinup, bench_prefix_io);
criterion_main!(benches);
