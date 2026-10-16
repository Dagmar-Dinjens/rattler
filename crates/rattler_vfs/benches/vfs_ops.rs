//! In-process `rattler_vfs` costs on a real environment, with no kernel mount:
//! building the metadata tree, constructing the `VirtualFS` (eager offset
//! computation), and serving lookups/reads through `VfsOps` directly.
//!
//! The difference between these numbers and the `spinup` mounted-I/O numbers
//! is the transport (NFS / FUSE / `ProjFS`) overhead.

mod common;

use std::{ffi::OsStr, hint::black_box, path::Path, time::Duration};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use rattler_vfs::{
    vfs_ops::{ContentSource, FileKind, VfsOps},
    virtual_fs::VirtualFS,
};

const ROOT_INO: u64 = 1;
const READ_SIZE: u32 = 1 << 20;

#[derive(Default)]
struct Inventory {
    entries: u64,
    files: Vec<u64>,
    transformed: Vec<u64>,
    bytes: u64,
    transformed_bytes: u64,
}

/// `readdir` + `lookup` every entry, recursively. Returns what it found so the
/// read benchmarks can reuse the inode list.
fn walk(vfs: &VirtualFS, ino: u64, inv: &mut Inventory) {
    let Ok(entries) = vfs.readdir(ino, 0) else {
        return;
    };
    for entry in entries {
        if entry.name == OsStr::new(".") || entry.name == OsStr::new("..") {
            continue;
        }
        let Ok(attr) = vfs.lookup(ino, &entry.name) else {
            continue;
        };
        inv.entries += 1;
        match entry.kind {
            FileKind::Directory => walk(vfs, attr.ino, inv),
            FileKind::RegularFile => {
                inv.files.push(attr.ino);
                inv.bytes += attr.size;
                if matches!(vfs.content_source(attr.ino), Ok(ContentSource::Transformed)) {
                    inv.transformed.push(attr.ino);
                    inv.transformed_bytes += attr.size;
                }
            }
            FileKind::Symlink => {}
        }
    }
}

fn read_whole(vfs: &VirtualFS, ino: u64) -> u64 {
    let mut offset = 0u64;
    loop {
        match vfs.read(ino, offset, READ_SIZE) {
            Ok(chunk) if !chunk.is_empty() => offset += chunk.len() as u64,
            _ => return offset,
        }
    }
}

fn mount_point() -> &'static Path {
    // Never mounted; only baked into prefix-replaced bytes.
    Path::new(if cfg!(windows) {
        r"C:\rattler-vfs-bench\env"
    } else {
        "/tmp/rattler-vfs-bench/env"
    })
}

fn bench_construction(c: &mut Criterion) {
    let fixture = common::fixture();
    let rt = common::runtime();
    let mut group = c.benchmark_group("vfs/construct");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(20));

    // Lock file -> tree: reads every package's paths.json from the cache.
    group.bench_function("metadata_tree_build", |b| {
        b.iter(|| rt.block_on(fixture.tree(mount_point())));
    });
    // Tree -> VirtualFS: eager prefix-offset scan of every placeholder file.
    group.bench_function("virtual_fs_new", |b| {
        b.iter_batched(
            || rt.block_on(fixture.tree(mount_point())),
            |tree| VirtualFS::from_tree(tree, mount_point()),
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

fn bench_ops(c: &mut Criterion) {
    let fixture = common::fixture();
    let rt = common::runtime();
    let vfs = VirtualFS::from_tree(rt.block_on(fixture.tree(mount_point())), mount_point());
    let mut inv = Inventory::default();
    walk(&vfs, ROOT_INO, &mut inv);
    eprintln!(
        "vfs/ops: {} entries, {} files ({} MiB), {} prefix-replaced files ({} MiB)",
        inv.entries,
        inv.files.len(),
        inv.bytes >> 20,
        inv.transformed.len(),
        inv.transformed_bytes >> 20
    );

    let mut group = c.benchmark_group("vfs/ops");
    group
        .sample_size(10)
        .measurement_time(Duration::from_secs(15));

    group.throughput(Throughput::Elements(inv.entries));
    group.bench_function("walk_readdir_lookup", |b| {
        b.iter(|| {
            let mut i = Inventory::default();
            walk(&vfs, ROOT_INO, &mut i);
            black_box(i.entries)
        });
    });

    group.throughput(Throughput::Elements(inv.files.len() as u64));
    group.bench_function("getattr_all_files", |b| {
        b.iter(|| {
            for &ino in &inv.files {
                black_box(vfs.getattr(ino).ok());
            }
        });
    });

    group.throughput(Throughput::Bytes(inv.bytes));
    group.bench_function("read_all_files", |b| {
        b.iter(|| {
            inv.files
                .iter()
                .map(|&ino| read_whole(&vfs, ino))
                .sum::<u64>()
        });
    });

    group.throughput(Throughput::Bytes(inv.transformed_bytes));
    group.bench_function("read_prefix_replaced_warm", |b| {
        b.iter(|| {
            inv.transformed
                .iter()
                .map(|&ino| read_whole(&vfs, ino))
                .sum::<u64>()
        });
    });
    // First read on a fresh VirtualFS: no mmap / codesign cache yet. On macOS
    // this includes ad-hoc re-signing of every prefix-replaced Mach-O.
    group.bench_function("read_prefix_replaced_cold", |b| {
        b.iter_batched(
            || {
                // Inode numbers are only stable within one tree, so re-list.
                let fresh =
                    VirtualFS::from_tree(rt.block_on(fixture.tree(mount_point())), mount_point());
                let mut fresh_inv = Inventory::default();
                walk(&fresh, ROOT_INO, &mut fresh_inv);
                (fresh, fresh_inv.transformed)
            },
            |(fresh, transformed)| {
                transformed
                    .iter()
                    .map(|&ino| read_whole(&fresh, ino))
                    .sum::<u64>()
            },
            BatchSize::PerIteration,
        );
    });
    group.finish();
}

criterion_group!(benches, bench_construction, bench_ops);
criterion_main!(benches);
