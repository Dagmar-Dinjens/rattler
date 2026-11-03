//! Scratch: where does mount-time placeholder scanning spend its time?
use std::time::{Duration, Instant};

use rattler_conda_types::{
    Subdir,
    package::{FileMode, PackageFile, PathsJson},
};
use rattler_vfs::prefix_replacement::{plan_binary_replacement, plan_text_replacement};

fn enc(p: &str) -> Vec<(&'static str, Vec<u8>)> {
    let u16le: Vec<u8> = p.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let u16be: Vec<u8> = p.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let u32le: Vec<u8> = p.chars().flat_map(|c| (c as u32).to_le_bytes()).collect();
    let u32be: Vec<u8> = p.chars().flat_map(|c| (c as u32).to_be_bytes()).collect();
    vec![("utf8", p.as_bytes().to_vec()), ("u16le", u16le), ("u16be", u16be), ("u32le", u32le), ("u32be", u32be)]
}

fn main() {
    let root = std::env::args().nth(1).expect("pkgs dir");
    let mut files = Vec::new();
    for pkg in std::fs::read_dir(&root).unwrap().flatten() {
        let Ok(paths) = PathsJson::from_package_directory(&pkg.path()) else { continue };
        for e in paths.paths {
            if let Some(ph) = e.prefix_placeholder {
                let data = std::fs::read(pkg.path().join(&e.relative_path)).unwrap_or_default();
                files.push((ph.file_mode, ph.placeholder, data, ph.experimental_offsets.is_some()));
            }
        }
    }
    let (mut nb, mut nt, mut bb, mut tb, mut rec) = (0, 0, 0usize, 0usize, 0);
    for (m, _, d, r) in &files { if *r { rec += 1 }; match m { FileMode::Binary => { nb += 1; bb += d.len() } _ => { nt += 1; tb += d.len() } } }
    println!("{} binary files ({} MiB), {} text files ({} MiB), {} with recorded offsets", nb, bb >> 20, nt, tb >> 20, rec);
    let reps = 5;
    let mut per_enc = vec![[Duration::ZERO; 2]; 5];
    let (mut plan_b, mut plan_t) = (Duration::ZERO, Duration::ZERO);
    for _ in 0..reps {
        for (m, ph, d, _) in &files {
            let k = usize::from(*m == FileMode::Binary);
            for (i, (_, needle)) in enc(ph).iter().enumerate() {
                let t = Instant::now();
                std::hint::black_box(memchr::memmem::find_iter(d, needle).count());
                per_enc[i][k] += t.elapsed();
            }
            let t = Instant::now();
            match m {
                FileMode::Binary => { std::hint::black_box(plan_binary_replacement(d, ph)); plan_b += t.elapsed() }
                _ => { std::hint::black_box(plan_text_replacement(d, ph, "/some/mount/point", &Subdir::current().unwrap())); plan_t += t.elapsed() }
            }
        }
    }
    let ms = |d: Duration| d.as_secs_f64() * 1e3 / reps as f64;
    for (i, name) in ["utf8", "u16le", "u16be", "u32le", "u32be"].iter().enumerate() {
        println!("memmem {name:6} text {:7.2} ms  binary {:7.2} ms", ms(per_enc[i][0]), ms(per_enc[i][1]));
    }
    // Candidate exact prefilters over the binaries.
    let core = |ph: &str, unit: usize| -> Vec<u8> {
        let b = ph.as_bytes();
        let mut v = Vec::new();
        for (i, c) in b.iter().enumerate() {
            v.push(*c);
            if i + 1 < b.len() { v.extend(std::iter::repeat_n(0u8, unit - 1)); }
        }
        v
    };
    let (mut t3, mut tac3, mut tac5) = (Duration::ZERO, Duration::ZERO, Duration::ZERO);
    let (mut hits3, mut hitsac) = (0usize, 0usize);
    for _ in 0..reps {
        for (m, ph, d, _) in &files {
            if *m != FileMode::Binary { continue }
            let needles = [ph.as_bytes().to_vec(), core(ph, 2), core(ph, 4)];
            let t = Instant::now();
            for n in &needles { hits3 += memchr::memmem::find_iter(d, n).count(); }
            t3 += t.elapsed();
            let ac = aho_corasick::AhoCorasick::new(&needles).unwrap();
            let t = Instant::now();
            hitsac += ac.find_overlapping_iter(d.as_slice()).count();
            tac3 += t.elapsed();
            let ac5 = aho_corasick::AhoCorasick::new(enc(ph).into_iter().map(|e| e.1)).unwrap();
            let t = Instant::now();
            std::hint::black_box(ac5.find_overlapping_iter(d.as_slice()).count());
            tac5 += t.elapsed();
        }
    }
    for kind in [None, Some(aho_corasick::AhoCorasickKind::NoncontiguousNFA), Some(aho_corasick::AhoCorasickKind::ContiguousNFA), Some(aho_corasick::AhoCorasickKind::DFA)] {
        let (mut tb, mut ts) = (Duration::ZERO, Duration::ZERO);
        for (m, ph, d, _) in &files {
            if *m != FileMode::Binary { continue }
            let needles = [ph.as_bytes().to_vec(), core(ph, 2), core(ph, 4)];
            let t = Instant::now();
            let ac = aho_corasick::AhoCorasick::builder().kind(kind).build(&needles).unwrap();
            tb += t.elapsed();
            let t = Instant::now();
            std::hint::black_box(ac.find_overlapping_iter(d.as_slice()).count());
            ts += t.elapsed();
        }
        println!("kind {kind:?}: build {:.2} ms, search {:.2} ms", tb.as_secs_f64()*1e3, ts.as_secs_f64()*1e3);
    }
    println!("3 core memmem passes {:.2} ms ({hits3} hits), aho-corasick 3 core overlapping {:.2} ms ({hitsac}), aho-corasick 5 full overlapping {:.2} ms", ms(t3), ms(tac3), ms(tac5));
    println!("plan_text_replacement total {:.2} ms, plan_binary_replacement total {:.2} ms (single-threaded)", ms(plan_t), ms(plan_b));
}
