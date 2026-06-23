use std::{path::PathBuf, sync::Arc};

use anyhow::Context;
use async_trait::async_trait;

use crate::{
    backends::nfs_fs::NfsFS,
    mount::{MountProvider, MountSession},
    virtual_fs_core::VirtualFSCore,
};

use nfs3_server::tcp::{NFSTcp, NFSTcpListener};

const NFS_ADDR: &str = "127.0.0.1:11111";

pub struct NfsProvider;

pub struct NfsSession {
    mount_point: PathBuf,
    server_thread: std::thread::JoinHandle<anyhow::Result<()>>,
}

impl MountSession for NfsSession {
    fn unmount(self: Box<Self>) -> anyhow::Result<()> {
        // Unmount the OS-level filesystem first, then drop the server.
        let status = std::process::Command::new("umount")
            .arg(&self.mount_point)
            .status()
            .context("failed to run umount")?;

        if !status.success() {
            eprintln!(
                "umount exited with {:?} — server still stopping",
                status.code()
            );
        }

        drop(self.server_thread);
        Ok(())
    }
}

#[async_trait]
impl MountProvider for NfsProvider {
    async fn mount(
        fs: Arc<VirtualFSCore>,
        mount_point: PathBuf,
    ) -> anyhow::Result<Box<dyn MountSession>> {
        let filesystem = NfsFS { inner: fs };
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();

        let server_thread = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;

            runtime.block_on(async move {
                let listener = match NFSTcpListener::bind(NFS_ADDR, filesystem).await {
                    Ok(listener) => {
                        let _ = ready_tx.send(Ok(()));
                        listener
                    }
                    Err(err) => {
                        let _ = ready_tx.send(Err(err.to_string()));
                        return Err(err.into());
                    }
                };

                listener.handle_forever().await.map_err(Into::into)
            })
        });

        ready_rx
            .recv()
            .map_err(|_| anyhow::anyhow!("NFS server thread exited before binding"))?
            .map_err(|err| anyhow::anyhow!("failed binding NFS listener: {err}"))?;

        eprintln!("NFS server listening on {NFS_ADDR}");

        // Mount the NFS export at the requested path.
        // Requires either root or appropriate system permissions.
        // The nfs3_server crate serves portmapper, mountd, and NFS3 all on the
        // same TCP port. We must specify both `port` and `mountport` so that
        // mount_nfs skips the system portmapper (port 111) entirely and talks
        // directly to our server for both the mount handshake and NFS I/O.
        #[cfg(target_os = "macos")]
        let status = std::process::Command::new("mount_nfs")
            .args([
                "-o",
                "vers=3,tcp,port=11111,mountport=11111,nolockd,nolock,soft,intr,noresvport",
            ])
            .arg("127.0.0.1:/")
            .arg(&mount_point)
            .status()
            .context("failed to run mount_nfs — try running with sudo")?;

        #[cfg(target_os = "linux")]
        let status = std::process::Command::new("mount")
            .args([
                "-t",
                "nfs",
                "-o",
                "vers=3,proto=tcp,port=11111,mountport=11111,mountproto=tcp,soft,nolock",
                "127.0.0.1:/",
            ])
            .arg(&mount_point)
            .status()
            .context("failed to run mount — try running with sudo")?;

        if !status.success() {
            return Err(anyhow::anyhow!(
                "mount command failed with exit code {:?}",
                status.code()
            ));
        }

        eprintln!("mounted at {}", mount_point.display());

        Ok(Box::new(NfsSession {
            mount_point,
            server_thread,
        }))
    }
}
