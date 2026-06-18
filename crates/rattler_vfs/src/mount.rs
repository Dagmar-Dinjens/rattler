use async_trait::async_trait;
use std::{path::PathBuf, sync::Arc};

use crate::{backends::nfs::NfsProvider, metadata::FSMetadata, virtual_fs_core::VirtualFSCore};

pub trait MountSession: Send + Sync {
    fn unmount(self: Box<Self>) -> anyhow::Result<()>;
}

#[derive(Debug, Clone, Copy)]
pub enum MountBackend {
    Nfs,
}

impl MountBackend {
    pub async fn mount(
        &self,
        metadata: Vec<FSMetadata>,
        mount_point: PathBuf,
    ) -> anyhow::Result<Box<dyn MountSession>> {
        let fs = Arc::new(VirtualFSCore::new(metadata, mount_point.clone()));

        match self {
            MountBackend::Nfs => NfsProvider::mount(fs, mount_point).await,
        }
    }
}
impl From<&str> for MountBackend {
    fn from(value: &str) -> Self {
        match value {
            "nfs" => MountBackend::Nfs,
            _ => MountBackend::Nfs,
        }
    }
}
#[async_trait]
pub trait MountProvider {
    async fn mount(
        fs: Arc<VirtualFSCore>,
        mount_point: PathBuf,
    ) -> anyhow::Result<Box<dyn MountSession>>;
}
