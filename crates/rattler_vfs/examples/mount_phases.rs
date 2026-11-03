//! Scratch: per-phase timing of a mount, to split `spinup/mounted`.
#[path = "../benches/common/mod.rs"]
mod common;

use std::time::Instant;

use rattler_vfs::{mount, virtual_fs::VirtualFS};

fn main() {
    let fixture = common::fixture();
    let rt = common::runtime();
    let base = tempfile::tempdir().unwrap();
    let n = 40;
    let mut phases: Vec<[f64; 5]> = Vec::new();
    for i in 0..n + 3 {
        let prefix = base.path().join("slot");
        if prefix.exists() {
            std::fs::remove_dir_all(&prefix).unwrap();
        }
        std::fs::create_dir_all(&prefix).unwrap();
        let t = Instant::now();
        let tree = rt.block_on(fixture.tree(&prefix));
        let t_tree = t.elapsed();
        let tree2 = rt.block_on(fixture.tree(&prefix));
        let t = Instant::now();
        let vfs = VirtualFS::from_tree(tree2, &prefix);
        let t_new = t.elapsed();
        drop(vfs);
        let t = Instant::now();
        let handle = rt.block_on(mount(tree, &fixture.mount_config(&prefix))).unwrap();
        let t_mount = t.elapsed();
        let t = Instant::now();
        std::fs::metadata(common::python_in(&prefix)).unwrap();
        let t_first = t.elapsed();
        let t = Instant::now();
        rt.block_on(handle.unmount()).unwrap();
        let t_umount = t.elapsed();
        if i >= 3 {
            let ms = |d: std::time::Duration| d.as_secs_f64() * 1e3;
            phases.push([ms(t_tree), ms(t_new), ms(t_mount), ms(t_first), ms(t_umount)]);
        }
    }
    let names = ["tree_build", "virtual_fs_new", "mount()", "first_stat", "unmount"];
    for (k, name) in names.iter().enumerate() {
        let mut v: Vec<f64> = phases.iter().map(|p| p[k]).collect();
        v.sort_by(f64::total_cmp);
        println!("{name:15} median {:7.2} ms  p10 {:7.2}  p90 {:7.2}", v[v.len() / 2], v[v.len() / 10], v[v.len() * 9 / 10]);
    }
    let mut v: Vec<f64> = phases.iter().map(|p| p[2] - p[1]).collect();
    v.sort_by(f64::total_cmp);
    println!("{:15} median {:7.2} ms", "mount()-new", v[v.len() / 2]);
}
