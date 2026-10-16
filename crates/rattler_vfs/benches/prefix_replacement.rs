//! Prefix-replacement hot paths, on synthetic files (no network, no mount).
//!
//! These are the byte transforms every mounted read of a prefix-bearing file
//! goes through: building the replacement plan when the VFS is constructed,
//! and splicing the target prefix into each read window.

use std::hint::black_box;

use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use rattler_conda_types::Platform;
use rattler_vfs::prefix_replacement::{
    binary_ranged_read, collect_binary_offsets, collect_offsets, plan_text_replacement,
    text_ranged_read,
};

/// A placeholder as long as the ones conda-build bakes in (255 bytes).
fn placeholder() -> String {
    let mut p =
        String::from("/home/conda/feedstock_root/build_artifacts/pkg_1700000000000/_h_env_");
    while p.len() < 255 {
        p.push_str("placehold_");
    }
    p.truncate(255);
    p
}

const TARGET: &str = "/Users/someone/.cache/rattler/cache/exec/python-3f2a9c1e-vfs";

const SIZES: &[(&str, usize)] = &[("4KiB", 4 << 10), ("256KiB", 256 << 10), ("8MiB", 8 << 20)];

/// Window the kernel asks for per read (FUSE `max_read` / NFS `rsize`).
const WINDOW: usize = 128 << 10;

/// Text file: a shebang line plus the placeholder roughly every 4 KiB.
fn text_file(size: usize, placeholder: &str) -> Vec<u8> {
    let mut out = format!("#!{placeholder}/bin/python3.12\n").into_bytes();
    let filler = b"import os, sys  # some ordinary python source line\n";
    while out.len() < size {
        for _ in 0..80 {
            out.extend_from_slice(filler);
        }
        out.extend_from_slice(placeholder.as_bytes());
        out.extend_from_slice(b"/lib/python3.12/site-packages\n");
    }
    out.truncate(size);
    out
}

/// Binary file: NUL-terminated C strings containing the placeholder every
/// 64 KiB in otherwise opaque bytes.
fn binary_file(size: usize, placeholder: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut x: u32 = 0x1234_5678;
    while out.len() < size {
        for _ in 0..(64 << 10) {
            // xorshift, avoiding NULs so only our strings terminate.
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            out.push((x as u8) | 1);
        }
        out.push(0);
        out.extend_from_slice(placeholder.as_bytes());
        out.extend_from_slice(b"/lib:");
        out.extend_from_slice(placeholder.as_bytes());
        out.extend_from_slice(b"/lib64\0");
    }
    out.truncate(size);
    out
}

fn bench_offsets(c: &mut Criterion) {
    let placeholder = placeholder();
    let mut group = c.benchmark_group("prefix_replacement/scan");
    for &(label, size) in SIZES {
        let text = text_file(size, &placeholder);
        let binary = binary_file(size, &placeholder);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::new("collect_offsets", label), &text, |b, t| {
            b.iter(|| collect_offsets(black_box(t), placeholder.as_bytes()));
        });
        group.bench_with_input(
            BenchmarkId::new("plan_text_replacement", label),
            &text,
            |b, t| {
                b.iter(|| {
                    plan_text_replacement(black_box(t), &placeholder, TARGET, &Platform::Linux64)
                });
            },
        );
        group.bench_with_input(
            BenchmarkId::new("collect_binary_offsets", label),
            &binary,
            |b, bin| {
                b.iter(|| collect_binary_offsets(black_box(bin), placeholder.as_bytes()));
            },
        );
    }
    group.finish();
}

fn bench_ranged_reads(c: &mut Criterion) {
    let placeholder = placeholder();
    let mut group = c.benchmark_group("prefix_replacement/read");
    for &(label, size) in SIZES {
        let text = text_file(size, &placeholder);
        let plan = plan_text_replacement(&text, &placeholder, TARGET, &Platform::Linux64);
        let binary = binary_file(size, &placeholder);
        let groups = collect_binary_offsets(&binary, placeholder.as_bytes());
        group.throughput(Throughput::Bytes(size as u64));

        // Whole file in WINDOW-sized reads, the way a transport serves `cat`.
        group.bench_function(BenchmarkId::new("text_windowed", label), |b| {
            b.iter(|| {
                let mut start = 0;
                let mut total = 0;
                loop {
                    let chunk = text_ranged_read(
                        black_box(&text),
                        placeholder.as_bytes(),
                        TARGET.as_bytes(),
                        &plan.body_offsets,
                        plan.region_end,
                        &plan.transformed_region,
                        start,
                        start + WINDOW,
                    );
                    if chunk.is_empty() {
                        break total;
                    }
                    total += chunk.len();
                    start += chunk.len();
                }
            });
        });
        group.bench_function(BenchmarkId::new("binary_windowed", label), |b| {
            b.iter(|| {
                let mut start = 0;
                while start < binary.len() {
                    let chunk = binary_ranged_read(
                        black_box(&binary),
                        placeholder.as_bytes(),
                        TARGET.as_bytes(),
                        &groups,
                        start,
                        start + WINDOW,
                    );
                    start += chunk.len().max(1);
                }
                start
            });
        });
        // Baseline: copying the same bytes without any transform.
        group.bench_function(BenchmarkId::new("memcpy_windowed", label), |b| {
            b.iter(|| {
                let mut total = 0;
                for chunk in black_box(&text).chunks(WINDOW) {
                    total += black_box(chunk.to_vec()).len();
                }
                total
            });
        });
    }
    group.finish();
}

criterion_group!(benches, bench_offsets, bench_ranged_reads);
criterion_main!(benches);
