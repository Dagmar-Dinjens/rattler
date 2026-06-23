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
            // Verify the server we just started is actually reachable.
            std::net::TcpStream::connect("127.0.0.1:11111")
                .context("NFS server not reachable on 127.0.0.1:11111 immediately after start")?;

            // Tell the system portmapper (if running) about our server so the
            // kernel NFS client can discover it on port 11111.
            let rpcbind_running = rpcbind_register(NFS_PORT);
            eprintln!("rpcbind registration: {}", if rpcbind_running { "ok" } else { "skipped (rpcbind not running)" });

            linux_nfs_mount(&mount_point)?;
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

/// Shell out to mount.nfs (which is setuid root on most distros) so we don't
/// need CAP_SYS_ADMIN ourselves. Options match the nfs3_server README example.
/// After any failure we also grab the last few dmesg lines to surface the real
/// kernel error that mount.nfs otherwise hides behind "failed to apply fstab
/// options".
#[cfg(target_os = "linux")]
fn linux_nfs_mount(mount_point: &std::path::Path) -> anyhow::Result<()> {
    // mount.nfs is setuid root on most distros; fall back to plain `mount -t nfs`.
    let output = std::process::Command::new("mount.nfs")
        .args([
            "-o",
            "noacl,nolock,vers=3,tcp,port=11111,mountport=11111,actimeo=120,addr=127.0.0.1",
            "127.0.0.1:/",
        ])
        .arg(mount_point)
        .output()
        .or_else(|_| {
            std::process::Command::new("mount")
                .args([
                    "-t", "nfs",
                    "-o",
                    "noacl,nolock,vers=3,tcp,port=11111,mountport=11111,actimeo=120,addr=127.0.0.1",
                    "127.0.0.1:/",
                ])
                .arg(mount_point)
                .output()
        })
        .context("failed to run mount.nfs / mount — is nfs-utils installed?")?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);

    // Grab dmesg to expose the actual kernel errno that mount.nfs masks.
    let dmesg = std::process::Command::new("dmesg")
        .args(["--notime", "-l", "err,warn"])
        .output()
        .ok()
        .map(|o| {
            let raw = String::from_utf8_lossy(&o.stdout);
            // Last 10 lines most relevant.
            raw.lines()
                .rev()
                .take(10)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();

    Err(anyhow::anyhow!(
        "mount.nfs failed (exit {:?})\nstdout: {}\nstderr: {}\ndmesg (errors):\n{}",
        output.status.code(),
        stdout.trim(),
        stderr.trim(),
        dmesg.trim(),
    ))
}

/// Register NFS3 (100003) and MOUNT (100005) with system rpcbind on port 111.
/// Returns true if rpcbind was reached, false if it is not running.
#[cfg(target_os = "linux")]
fn rpcbind_register(port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let Ok(mut stream) = TcpStream::connect_timeout(
        &"127.0.0.1:111".parse().unwrap(),
        Duration::from_secs(1),
    ) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    for prog in [100003u32, 100005] {
        let msg = pmap_request(1, prog, 3, port);
        if stream.write_all(&msg).is_err() {
            return false;
        }
        let mut hdr = [0u8; 4];
        if stream.read_exact(&mut hdr).is_err() {
            return false;
        }
        let body_len = (u32::from_be_bytes(hdr) & 0x7FFF_FFFF) as usize;
        let mut body = vec![0u8; body_len];
        let _ = stream.read_exact(&mut body);
    }
    true
}

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

/// Build a TCP-framed portmapper RPC call (PMAPPROC_SET=1 or PMAPPROC_UNSET=2).
#[cfg(target_os = "linux")]
fn pmap_request(proc: u32, prog: u32, vers: u32, port: u16) -> Vec<u8> {
    let mut payload = Vec::with_capacity(56);
    let mut w = |n: u32| payload.extend_from_slice(&n.to_be_bytes());

    w(prog); w(0); w(2); w(100_000); w(2); w(proc);
    w(0); w(0); // cred: AUTH_NULL
    w(0); w(0); // verf: AUTH_NULL
    w(prog); w(vers); w(6); w(port as u32); // mapping: prog, vers, IPPROTO_TCP, port

    let mut msg = Vec::with_capacity(4 + payload.len());
    msg.extend_from_slice(&((payload.len() as u32) | 0x8000_0000).to_be_bytes());
    msg.extend_from_slice(&payload);
    msg
}
