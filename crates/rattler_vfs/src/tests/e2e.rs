#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf, time::Duration};

    use tempfile::tempdir;

    use crate::{MountBackend, mount_environment};

    #[compio::test]
    async fn test_prefix_replacement_file() -> anyhow::Result<()> {
        let mount_dir = tempfile::tempdir()?;

        let session = mount_environment(
            PathBuf::from("tests/data/pixi.lock"),
            PathBuf::from("tests/data/cache"),
            mount_dir.path().to_path_buf(),
            MountBackend::Fuse,
            "default".into(),
        )
        .await?;

        std::thread::sleep(std::time::Duration::from_millis(500));

        let contents = std::fs::read_to_string(mount_dir.path().join("lib/pkgconfig/foo.pc"))?;

        assert!(!contents.contains("/opt/anaconda1anaconda2anaconda3"));
        assert!(contents.contains(mount_dir.path().to_str().unwrap()));

        session.unmount()?;

        Ok(())
    }

    #[compio::test]
    async fn test_prefix_replacement_python() -> anyhow::Result<()> {
        let mount_dir = tempfile::tempdir()?;

        let session = mount_environment(
            PathBuf::from("tests/data/pixi.lock"),
            PathBuf::from("tests/data/cache"),
            mount_dir.path().to_path_buf(),
            MountBackend::Fuse,
            "default".into(),
        )
        .await?;

        std::thread::sleep(std::time::Duration::from_millis(500));

        let output = std::process::Command::new(mount_dir.path().join("bin/python"))
            .arg("-c")
            .arg("import sys; print(sys.prefix)")
            .output()?;

        assert!(output.status.success());

        let prefix = String::from_utf8(output.stdout)?;

        assert!(prefix.trim() == mount_dir.path().to_str().unwrap());

        session.unmount()?;

        Ok(())
    }
}
