use crate::prefix_replacement::{binary_prefix_replacement, text_prefix_replacement};
use anyhow::anyhow;
use rattler_conda_types::package::{FileMode, PathType};
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
    pub is_symlink: bool,
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
    pub is_symlink: bool,
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

    /// Converts a 1-based inode number (as exposed to NFS/callers) to a 0-based metadata index.
    fn ino_to_index(&self, ino: usize) -> Option<usize> {
        if ino == 0 {
            return None;
        }
        let idx = ino - 1;
        (idx < self.metadata.len()).then_some(idx)
    }

    fn index_to_ino(&self, idx: usize) -> u64 {
        (idx + 1) as u64
    }

    fn get_path(&self, file: &FSFile) -> PathBuf {
        let path = (*file.cache_base_path).to_path_buf();
        let parent = self.metadata[file.parent].as_directory().unwrap();
        path.join(&parent.prefix_path).join(&file.file_name)
    }

    /// `parent_ino` is a 1-based inode number. Returns the 0-based metadata index of the child.
    pub fn lookup(&self, parent_ino: usize, name: &OsStr) -> Option<usize> {
        let parent_idx = self.ino_to_index(parent_ino)?;
        let parent_dir = self.metadata.get(parent_idx)?.as_directory()?;

        parent_dir.children.iter().find_map(|child_idx| {
            let child = &self.metadata[*child_idx];
            (child.file_name() == name).then_some(*child_idx)
        })
    }

    /// `ino` is a 1-based inode number.
    pub fn getattr(&self, ino: usize) -> anyhow::Result<VirtualAttr> {
        let idx = self
            .ino_to_index(ino)
            .ok_or_else(|| anyhow!("invalid ino {ino}"))?;

        let entry = &self.metadata[idx];

        match entry {
            FSMetadata::FSDirectory(_) => Ok(VirtualAttr {
                is_dir: true,
                is_symlink: false,
                size: 0,
                perm: 0o755,
                uid: unsafe { libc::getuid() },
                gid: unsafe { libc::getgid() },
            }),

            FSMetadata::FSFile(file) => {
                let is_symlink = file.path_type == PathType::SoftLink;
                let path = self.get_path(file);
                let meta = std::fs::metadata(&path)?;

                Ok(VirtualAttr {
                    is_dir: false,
                    is_symlink,
                    size: meta.len(),
                    perm: if is_symlink {
                        0o777
                    } else {
                        (meta.permissions().mode() & 0o777) as u16
                    },
                    uid: unsafe { libc::getuid() },
                    gid: unsafe { libc::getgid() },
                })
            }
        }
    }

    pub fn open(&self, ino: usize) -> anyhow::Result<u64> {
        self.open_cached(ino)
    }

    /// `ino` is a 1-based inode number.
    pub fn open_cached(&self, ino: usize) -> anyhow::Result<u64> {
        let idx = self
            .ino_to_index(ino)
            .ok_or_else(|| anyhow!("invalid ino {ino}"))?;

        {
            let handles = self.open_handles.lock().unwrap();
            if let Some(&fh) = handles.get(&idx) {
                return Ok(fh);
            }
        }

        let file = self.metadata[idx]
            .as_file()
            .ok_or_else(|| anyhow!("not a file"))?;

        let path = self.get_path(file);
        let fd = File::open(path)?;
        let mmap = unsafe { Mmap::map(&fd)? };

        let fh = self.next_fh.fetch_add(1, Ordering::Relaxed);

        self.open_files.lock().unwrap().insert(fh, mmap);
        self.open_handles.lock().unwrap().insert(idx, fh);

        Ok(fh)
    }

    pub fn release(&self, fh: u64) {
        self.open_files.lock().unwrap().remove(&fh);

        let mut handles = self.open_handles.lock().unwrap();
        handles.retain(|_, v| *v != fh);
    }

    /// `ino` is a 1-based inode number.
    pub fn read(&self, ino: usize, fh: u64, offset: usize, size: usize) -> anyhow::Result<Vec<u8>> {
        let idx = self
            .ino_to_index(ino)
            .ok_or_else(|| anyhow!("invalid ino {ino}"))?;

        let file_meta = self.metadata[idx]
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

    /// `ino` is a 1-based inode number. Returns the symlink target bytes.
    pub fn readlink(&self, ino: usize) -> anyhow::Result<Vec<u8>> {
        let idx = self
            .ino_to_index(ino)
            .ok_or_else(|| anyhow!("invalid ino {ino}"))?;

        let file = self.metadata[idx]
            .as_file()
            .ok_or_else(|| anyhow!("not a file"))?;

        if file.path_type != PathType::SoftLink {
            return Err(anyhow!("not a symlink"));
        }

        Ok(std::fs::read(self.get_path(file))?)
    }

    /// `ino` is a 1-based inode number.
    pub fn readdir(&self, ino: usize) -> anyhow::Result<Vec<DirectoryEntry>> {
        let idx = self
            .ino_to_index(ino)
            .ok_or_else(|| anyhow!("invalid ino {ino}"))?;

        let dir = self.metadata[idx]
            .as_directory()
            .ok_or_else(|| anyhow!("not a directory"))?;

        Ok(dir
            .children
            .iter()
            .map(|child_idx| {
                let child = &self.metadata[*child_idx];

                DirectoryEntry {
                    ino: self.index_to_ino(*child_idx),
                    name: child.file_name().to_os_string(),
                    is_dir: matches!(child, FSMetadata::FSDirectory(_)),
                    is_symlink: matches!(child, FSMetadata::FSFile(f) if f.path_type == PathType::SoftLink),
                }
            })
            .collect())
    }
}
