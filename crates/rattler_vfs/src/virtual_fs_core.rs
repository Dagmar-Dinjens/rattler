use crate::prefix_replacement::{binary_prefix_replacement, text_prefix_replacement};
use anyhow::anyhow;
use rattler_conda_types::package::FileMode;
use std::{
    collections::HashMap,
    ffi::{OsStr, OsString},
    fs::File,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

use memmap2::Mmap;

use crate::metadata::{FSFile, FSMetadata};

pub struct VirtualFSCore {
    metadata: Vec<FSMetadata>,
    mount_point: PathBuf,
    open_files: Mutex<HashMap<u64, Mmap>>,
    open_handles: Mutex<HashMap<usize, u64>>,
    next_fh: AtomicU64,
}

#[derive(Debug, Clone)]
pub struct VirtualAttr {
    pub is_dir: bool,
    pub size: u64,
    pub perm: u16,
    pub uid: u32,
    pub gid: u32,
}

#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub ino: u64,
    pub name: OsString,
    pub is_dir: bool,
}

impl VirtualFSCore {
    pub fn new(metadata: Vec<FSMetadata>, mount_point: PathBuf) -> Self {
        Self {
            metadata,
            mount_point,
            open_files: Mutex::new(HashMap::new()),
            open_handles: Mutex::new(HashMap::new()),
            next_fh: AtomicU64::new(1),
        }
    }

    fn get_path(&self, file: &FSFile) -> PathBuf {
        let mut path = (*file.cache_base_path).to_path_buf();

        let parent = self.metadata[file.parent].as_directory().unwrap();

        path = path.join(&parent.prefix_path);

        path.join(&file.file_name)
    }
    pub fn lookup(&self, parent: usize, name: &OsStr) -> Option<usize> {
        let parent_dir = self.metadata.get(parent)?.as_directory()?;

        parent_dir
            .children
            .iter()
            .find(|child_idx| self.metadata[**child_idx].file_name() == name)
            .copied()
    }
    pub fn getattr(&self, ino: usize) -> anyhow::Result<VirtualAttr> {
        let entry = &self.metadata[ino];

        match entry {
            FSMetadata::FSDirectory(_) => Ok(VirtualAttr {
                is_dir: true,
                size: 0,
                perm: 0o755,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
            }),

            FSMetadata::FSFile(file) => {
                let path = self.get_path(file);

                let meta = std::fs::metadata(path)?;

                Ok(VirtualAttr {
                    is_dir: false,
                    size: meta.len(),
                    perm: (meta.permissions().mode() & 0o777) as u16,
                    uid: unsafe { libc::getuid() },
                    gid: unsafe { libc::getgid() },
                })
            }
        }
    }
    pub fn open_cached(&self, ino: usize) -> anyhow::Result<u64> {
        // Return existing handle if this inode is already open
        if let Some(fh) = self.open_handles.lock().unwrap().get(&ino) {
            return Ok(*fh);
        }

        let file = self.metadata[ino]
            .as_file()
            .ok_or_else(|| anyhow!("not a file"))?;

        let path = self.get_path(file);

        let fd = File::open(path)?;

        let mmap = unsafe { Mmap::map(&fd)? };

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);

        self.open_files.lock().unwrap().insert(fh, mmap);

        self.open_handles.lock().unwrap().insert(ino, fh);

        Ok(fh)
    }
    pub fn readdir(&self, ino: usize) -> anyhow::Result<Vec<DirectoryEntry>> {
        let dir = self.metadata[ino]
            .as_directory()
            .ok_or_else(|| anyhow!("not a directory"))?;

        let mut entries = Vec::new();

        for child_idx in &dir.children {
            let child = &self.metadata[*child_idx];

            entries.push(DirectoryEntry {
                ino: (*child_idx + 1) as u64,
                name: child.file_name().to_os_string(),
                is_dir: matches!(child, FSMetadata::FSDirectory(_)),
            });
        }

        Ok(entries)
    }
    pub fn open(&self, ino: usize) -> anyhow::Result<u64> {
        let file = self.metadata[ino]
            .as_file()
            .ok_or_else(|| anyhow!("not a file"))?;

        let path = self.get_path(file);

        let fd = File::open(path)?;

        let mmap = unsafe { Mmap::map(&fd)? };

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);

        self.open_files.lock().unwrap().insert(fh, mmap);

        Ok(fh)
    }
    pub fn release(&self, fh: u64) {
        self.open_files.lock().unwrap().remove(&fh);

        self.open_handles
            .lock()
            .unwrap()
            .retain(|_, cached_fh| *cached_fh != fh);
    }
    pub fn read(&self, ino: usize, fh: u64, offset: usize, size: usize) -> anyhow::Result<Vec<u8>> {
        let file_meta = self.metadata[ino]
            .as_file()
            .ok_or_else(|| anyhow!("not a file"))?;

        let open_files = self.open_files.lock().unwrap();

        let mmap = open_files.get(&fh).ok_or_else(|| anyhow!("invalid fh"))?;

        if offset >= mmap.len() {
            return Ok(vec![]);
        }

        let end = offset.saturating_add(size).min(mmap.len());

        match &file_meta.prefix_placeholder {
            Some(placeholder) => match placeholder.file_mode {
                FileMode::Text => Ok(text_prefix_replacement(
                    placeholder,
                    offset,
                    end,
                    size,
                    mmap,
                    &self.mount_point,
                )),
                FileMode::Binary => Ok(binary_prefix_replacement(
                    placeholder,
                    offset,
                    end,
                    size,
                    mmap,
                    &self.mount_point,
                )),
            },

            None => Ok(mmap[offset..end].to_vec()),
        }
    }
}
