//! Linux descriptor-relative confinement for untrusted paths beneath a
//! configured root.  This intentionally fails closed if the required kernel
//! primitive is unavailable instead of falling back to pathname validation.

use std::{
    fs::File,
    os::unix::{ffi::OsStrExt, fs::MetadataExt, io::AsRawFd},
    path::{Component, Path, PathBuf},
};

use anyhow::{Context, Result, bail};

#[cfg(target_os = "linux")]
use rustix::{
    fs::{CWD, Mode, OFlags, ResolveFlags, openat2},
    io::Errno,
};

/// A directory descriptor that remains valid even if its pathname is renamed
/// or replaced.  All descendants must be opened relative to this descriptor.
pub struct SecureDir {
    file: File,
}

impl SecureDir {
    #[cfg(target_os = "linux")]
    pub fn open_root(path: &Path) -> Result<Self> {
        if path.as_os_str().as_bytes().contains(&0) {
            bail!("configured root contains a NUL byte");
        }
        let fd = openat2(
            CWD,
            path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::NO_SYMLINKS | ResolveFlags::NO_MAGICLINKS,
        )
        .map_err(|error| {
            if matches!(error, Errno::NOSYS) {
                anyhow::anyhow!("secure descriptor opens require Linux openat2 support")
            } else {
                anyhow::Error::from(error)
            }
        })
        .with_context(|| format!("securely open root {}", path.display()))?;
        let file = File::from(fd);
        if !file.metadata()?.is_dir() {
            bail!("configured root is not a directory");
        }
        Ok(Self { file })
    }

    #[cfg(not(target_os = "linux"))]
    pub fn open_root(_path: &Path) -> Result<Self> {
        bail!("secure local path access requires Linux openat2 support")
    }

    #[cfg(target_os = "linux")]
    fn open_relative(&self, path: &Path, flags: OFlags) -> Result<File> {
        validate_relative(path)?;
        let fd = openat2(
            &self.file,
            path,
            flags | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
            ResolveFlags::BENEATH
                | ResolveFlags::NO_SYMLINKS
                | ResolveFlags::NO_MAGICLINKS
                | ResolveFlags::NO_XDEV,
        )
        .map_err(anyhow::Error::from)
        .with_context(|| format!("securely open {} beneath root", path.display()))?;
        Ok(File::from(fd))
    }

    #[cfg(not(target_os = "linux"))]
    fn open_relative(&self, _path: &Path, _flags: ()) -> Result<File> {
        bail!("secure local path access requires Linux openat2 support")
    }

    pub fn open_file(&self, relative: &Path) -> Result<File> {
        #[cfg(target_os = "linux")]
        let file = self.open_relative(relative, OFlags::RDONLY)?;
        #[cfg(not(target_os = "linux"))]
        let file = self.open_relative(relative, ())?;
        validate_regular_single_link(&file)?;
        Ok(file)
    }

    pub fn open_directory(&self, relative: &Path) -> Result<Self> {
        #[cfg(target_os = "linux")]
        let file = self.open_relative(relative, OFlags::RDONLY | OFlags::DIRECTORY)?;
        #[cfg(not(target_os = "linux"))]
        let file = self.open_relative(relative, ())?;
        if !file.metadata()?.is_dir() {
            bail!("path is not a directory");
        }
        Ok(Self { file })
    }

    pub fn proc_path(&self) -> PathBuf {
        PathBuf::from(format!("/proc/self/fd/{}", self.file.as_raw_fd()))
    }
}

pub fn open_regular_file_beneath(root: &Path, relative: &Path) -> Result<File> {
    SecureDir::open_root(root)?.open_file(relative)
}

/// Report only a genuine missing-path failure. Callers may use this to
/// distinguish an active job whose log has not appeared yet from a path that
/// failed the confinement or file-type checks.
pub fn is_missing(error: &anyhow::Error) -> bool {
    #[cfg(target_os = "linux")]
    {
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<Errno>())
            .any(|errno| *errno == Errno::NOENT)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = error;
        false
    }
}

fn validate_relative(path: &Path) -> Result<()> {
    let mut parts = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(_) => parts += 1,
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                bail!("path must be a relative path below the configured root")
            }
        }
    }
    if parts == 0 {
        bail!("path must name a file below the configured root");
    }
    Ok(())
}

fn validate_regular_single_link(file: &File) -> Result<()> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!("path is not a regular file");
    }
    if metadata.nlink() != 1 {
        bail!("refusing a hard-linked file beneath the configured root");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn rejects_hard_links_and_symlink_parents() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("secret"), b"secret").unwrap();
        fs::hard_link(outside.join("secret"), root.join("hard.log")).unwrap();
        assert!(open_regular_file_beneath(&root, Path::new("hard.log")).is_err());

        symlink(&outside, root.join("linked")).unwrap();
        assert!(open_regular_file_beneath(&root, Path::new("linked/secret")).is_err());
    }

    #[test]
    fn missing_paths_are_distinct_from_confinement_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        let missing = open_regular_file_beneath(&root, Path::new("pending.log")).unwrap_err();
        assert!(is_missing(&missing));

        fs::write(temporary.path().join("outside"), b"secret").unwrap();
        fs::hard_link(
            temporary.path().join("outside"),
            root.join("hard-linked.log"),
        )
        .unwrap();
        let hard_link = open_regular_file_beneath(&root, Path::new("hard-linked.log")).unwrap_err();
        assert!(!is_missing(&hard_link));
    }

    #[test]
    fn pinned_root_rejects_a_parent_swapped_after_open() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        let outside = temporary.path().join("outside");
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(root.join("nested/log"), b"safe").unwrap();
        fs::write(outside.join("log"), b"secret").unwrap();
        let pinned = SecureDir::open_root(&root).unwrap();
        fs::rename(root.join("nested"), root.join("nested-old")).unwrap();
        symlink(&outside, root.join("nested")).unwrap();
        assert!(pinned.open_file(Path::new("nested/log")).is_err());
    }
}
