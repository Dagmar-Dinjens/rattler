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

struct Mounted {
    // Field order matters: unmount before the mount point directory goes.
    handle: Option<MountHandle>,
    dir: TempDir,
}

impl Mounted {
    fn prefix(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }
}

impl Drop for Mounted {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = common::runtime().block_on(handle.unmount());
        }
    }
}

fn mount() -> Mounted {
    let fixture = common::fixture();
    let dir = tempfile::tempdir().expect("tempdir");
    let handle = common::runtime()
        .block_on(build_and_mount(
            &fixture.lockfile,
            &fixture.environment,
            fixture.platform,
            &fixture.package_cache,
            &fixture.mount_config(dir.path()),
        ))
        .expect("mount environment");
    // A mount is only usable once the kernel answers for it.
    std::fs::metadata(common::python_in(dir.path())).expect("python visible in mount");
    Mounted {
        handle: Some(handle),
        dir,
    }
}

fn bench_spinup(c: &mut Criterion) {
    common::fixture();
    let mounts = common::mounts_enabled();
    let mut group = c.benchmark_group("spinup");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(30));

    group.bench_function("installed", |b| {
        b.iter_batched(
            || tempfile::tempdir().expect("tempdir"),
            |dir| {
                install(dir.path());
                dir
            },
            BatchSize::PerIteration,
        );
    });
    group.bench_function("installed_python", |b| {
        b.iter_batched(
            || tempfile::tempdir().expect("tempdir"),
            |dir| {
                install(dir.path());
                run_python(dir.path(), "pass");
                dir
            },
            BatchSize::PerIteration,
        );
    });
    if mounts {
        group.bench_function("mounted", |b| b.iter(mount));
        group.bench_function("mounted_python", |b| {
            b.iter(|| {
                let m = mount();
                run_python(&m.prefix(), "pass");
                m
            });
        });
        // Full lifecycle, including the unmount the other variants leave out.
        group.bench_function("mounted_and_unmounted", |b| {
            b.iter(|| drop(mount()));
        });
    }
    group.finish();
}

fn bench_prefix_io(c: &mut Criterion) {
    common::fixture();
    let installed = tempfile::tempdir().expect("tempdir");
    install(installed.path());
    let mounted = common::mounts_enabled().then(mount);

    let mut prefixes = vec![("installed", installed.path().to_path_buf())];
    if let Some(m) = &mounted {
        prefixes.push(("mounted", m.prefix()));
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
