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
#[cfg(target_os = "linux")]
const NFS_PORT: u16 = 11111;

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

        #[cfg(target_os = "linux")]
        rpcbind_unregister(NFS_PORT);

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
        // the client skips the system portmapper (port 111) and talks directly
        // to our server. `noresvport` avoids requiring a privileged source port.
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
        {
            // Register our NFS3 and MOUNT programs with the system portmapper so
            // the kernel NFS client can find our server even in text-based-options
            // mode, where the kernel queries rpcbind on port 111 for service
            // discovery rather than using our explicit port= option directly.
            rpcbind_register(NFS_PORT);

            let output = std::process::Command::new("mount.nfs")
                .args([
                    "-v",
                    "-o",
                    "noacl,nolock,vers=3,tcp,port=11111,mountport=11111,soft,noresvport",
                    "127.0.0.1:/",
                ])
                .arg(&mount_point)
                .output()
                .or_else(|_| {
                    // Fall back to `mount -t nfs` if mount.nfs is not in PATH.
                    std::process::Command::new("mount")
                        .args([
                            "-t",
                            "nfs",
                            "-v",
                            "-o",
                            "noacl,nolock,vers=3,tcp,port=11111,mountport=11111,soft,noresvport",
                            "127.0.0.1:/",
                        ])
                        .arg(&mount_point)
                        .output()
                })
                .context("failed to run mount.nfs / mount — try running with sudo")?;

            if !output.status.success() {
                rpcbind_unregister(NFS_PORT);
                let stderr = String::from_utf8_lossy(&output.stderr);
                return Err(anyhow::anyhow!(
                    "mount command failed with exit code {:?}: {}",
                    output.status.code(),
                    stderr.trim()
                ));
            }
        }

        #[cfg(not(target_os = "linux"))]
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

/// Register NFS3 (program 100003) and MOUNT (program 100005) with the system
/// portmapper (rpcbind on port 111) so the kernel NFS client can locate our
/// server without needing to resolve via the default NFS port (2049).
///
/// Silently does nothing if rpcbind is not running.
#[cfg(target_os = "linux")]
fn rpcbind_register(port: u16) {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let Ok(mut stream) = TcpStream::connect_timeout(
        &"127.0.0.1:111".parse().unwrap(),
        Duration::from_secs(1),
    ) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    // NFS3 = program 100003 version 3, MOUNT = program 100005 version 3
    for prog in [100003u32, 100005] {
        let msg = pmap_set_request(prog, 3, port);
        if stream.write_all(&msg).is_err() {
            return;
        }
        // Drain the reply (we don't need the result — if rpcbind rejects us,
        // the explicit port= options in the mount command act as fallback).
        let mut hdr = [0u8; 4];
        if stream.read_exact(&mut hdr).is_err() {
            return;
        }
        let body_len = (u32::from_be_bytes(hdr) & 0x7FFF_FFFF) as usize;
        let mut body = vec![0u8; body_len];
        let _ = stream.read_exact(&mut body);
    }
}

/// Unregister NFS3 and MOUNT programs from rpcbind. Called on unmount so that
/// stale entries don't block the next session's registration.
#[cfg(target_os = "linux")]
fn rpcbind_unregister(port: u16) {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let Ok(mut stream) = TcpStream::connect_timeout(
        &"127.0.0.1:111".parse().unwrap(),
        Duration::from_secs(1),
    ) else {
        return;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    for prog in [100003u32, 100005] {
        // PMAPPROC_UNSET = procedure 2
        let msg = pmap_request(2, prog, 3, port);
        if stream.write_all(&msg).is_err() {
            return;
        }
        let mut hdr = [0u8; 4];
        if stream.read_exact(&mut hdr).is_err() {
            return;
        }
        let body_len = (u32::from_be_bytes(hdr) & 0x7FFF_FFFF) as usize;
        let mut body = vec![0u8; body_len];
        let _ = stream.read_exact(&mut body);
    }
}

/// Build a TCP-framed portmapper RPC request for PMAPPROC_SET (proc=1) or
/// PMAPPROC_UNSET (proc=2).
#[cfg(target_os = "linux")]
fn pmap_set_request(prog: u32, vers: u32, port: u16) -> Vec<u8> {
    pmap_request(1, prog, vers, port)
}

#[cfg(target_os = "linux")]
fn pmap_request(proc: u32, prog: u32, vers: u32, port: u16) -> Vec<u8> {
    let mut payload = Vec::with_capacity(56);
    let mut w = |n: u32| payload.extend_from_slice(&n.to_be_bytes());

    w(prog);        // XID — use program number for easy correlation
    w(0);           // msg_type: CALL
    w(2);           // rpcvers: 2
    w(100_000);     // portmapper program
    w(2);           // portmapper version
    w(proc);        // PMAPPROC_SET=1 or PMAPPROC_UNSET=2
    w(0); w(0);     // cred: AUTH_NULL, len=0
    w(0); w(0);     // verf: AUTH_NULL, len=0
    // struct mapping { prog, vers, prot=IPPROTO_TCP=6, port }
    w(prog);
    w(vers);
    w(6);           // IPPROTO_TCP
    w(port as u32);

    // TCP record-marking header: last-fragment bit set, followed by length
    let mut msg = Vec::with_capacity(4 + payload.len());
    msg.extend_from_slice(&((payload.len() as u32) | 0x8000_0000).to_be_bytes());
    msg.extend_from_slice(&payload);
    msg
}
