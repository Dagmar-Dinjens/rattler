use std::{ffi::OsStr, os::unix::ffi::OsStrExt, sync::Arc};

use nfs3_server::vfs::{
    FileHandleU64, NextResult, NfsReadFileSystem, ReadDirIterator, ReadDirPlusIterator,
};
use nfs3_types::nfs3::{
    Nfs3Option, entryplus3, fattr3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, post_op_attr,
};

use crate::virtual_fs_core::{DirectoryEntry, VirtualAttr, VirtualFSCore};

pub struct NfsFS {
    pub inner: Arc<VirtualFSCore>,
}

fn to_fattr(attr: VirtualAttr, ino: u64) -> fattr3 {
    fattr3 {
        type_: if attr.is_dir {
            ftype3::NF3DIR
        } else {
            ftype3::NF3REG
        },
        mode: attr.perm as u32,
        nlink: 1,
        uid: attr.uid,
        gid: attr.gid,
        size: attr.size,
        used: attr.size,
        rdev: Default::default(),
        fsid: 1,
        fileid: ino,
        atime: nfstime3::default(),
        mtime: nfstime3::default(),
        ctime: nfstime3::default(),
    }
}

fn nfs_error(e: anyhow::Error) -> nfsstat3 {
    eprintln!("NFS error: {e:?}");
    nfsstat3::NFS3ERR_IO
}

impl NfsReadFileSystem for NfsFS {
    type Handle = FileHandleU64;

    fn root_dir(&self) -> FileHandleU64 {
        FileHandleU64::new(1)
    }

    async fn lookup(
        &self,
        dirid: &FileHandleU64,
        filename: &filename3<'_>,
    ) -> Result<FileHandleU64, nfsstat3> {
        let parent = (dirid.as_u64() - 1) as usize;
        let name = OsStr::from_bytes(filename.as_ref());
        let child = self
            .inner
            .lookup(parent, name)
            .ok_or(nfsstat3::NFS3ERR_NOENT)?;

        Ok(FileHandleU64::new((child + 1) as u64))
    }

    async fn getattr(&self, id: &FileHandleU64) -> Result<fattr3, nfsstat3> {
        let ino = (id.as_u64() - 1) as usize;
        let attr = self.inner.getattr(ino).map_err(nfs_error)?;

        Ok(to_fattr(attr, id.as_u64()))
    }

    async fn read(
        &self,
        id: &FileHandleU64,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let ino = (id.as_u64() - 1) as usize;
        let fh = self.inner.open_cached(ino).map_err(nfs_error)?;
        let data = self
            .inner
            .read(ino, fh, offset as usize, count as usize)
            .map_err(nfs_error)?;
        let eof = data.len() < count as usize;

        Ok((data, eof))
    }

    async fn readdir(
        &self,
        dirid: &FileHandleU64,
        cookie: u64,
    ) -> Result<impl ReadDirIterator, nfsstat3> {
        self.readdirplus(dirid, cookie).await
    }

    async fn readdirplus(
        &self,
        dirid: &FileHandleU64,
        cookie: u64,
    ) -> Result<impl ReadDirPlusIterator, nfsstat3> {
        let ino = (dirid.as_u64() - 1) as usize;
        let entries = self.inner.readdir(ino).map_err(nfs_error)?;

        Ok(NfsDirectoryIterator::new(
            self.inner.clone(),
            entries,
            cookie as usize,
        ))
    }

    async fn readlink(&self, _id: &FileHandleU64) -> Result<nfspath3<'_>, nfsstat3> {
        Err(nfsstat3::NFS3ERR_INVAL)
    }
}

struct NfsDirectoryIterator {
    inner: Arc<VirtualFSCore>,
    entries: Vec<DirectoryEntry>,
    index: usize,
}

impl NfsDirectoryIterator {
    fn new(inner: Arc<VirtualFSCore>, entries: Vec<DirectoryEntry>, start: usize) -> Self {
        Self {
            inner,
            entries,
            index: start,
        }
    }
}

impl ReadDirPlusIterator for NfsDirectoryIterator {
    async fn next(&mut self) -> NextResult<entryplus3<'static>> {
        let Some(entry) = self.entries.get(self.index) else {
            return NextResult::Eof;
        };
        self.index += 1;

        let attr = self
            .inner
            .getattr((entry.ino - 1) as usize)
            .ok()
            .map(|attr| to_fattr(attr, entry.ino));

        NextResult::Ok(entryplus3 {
            fileid: entry.ino,
            name: filename3::from(entry.name.as_bytes().to_vec()),
            cookie: self.index as u64,
            name_attributes: attr.map_or(post_op_attr::None, post_op_attr::Some),
            name_handle: Nfs3Option::None,
        })
    }
}
