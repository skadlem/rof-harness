//! Fingerprint Git-visible target inputs without following links or reading
//! ignored build output. Hashes identify inputs; they are not security proofs.
use std::io::Read;
use std::path::Path;
use std::process::Command;

pub(crate) struct TargetState {
    pub dirty: bool,
    pub hash: String,
}

pub(crate) fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

struct Fingerprint(u64);
impl Fingerprint {
    fn update(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= *byte as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
}

/// None means provenance could not be established (non-repository or an
/// unreadable input), rather than a fingerprint of silently missing evidence.
pub(crate) fn snapshot(root: &Path) -> Option<TargetState> {
    let status = git(root, &["status", "--porcelain", "--untracked-files=normal"])?;
    let names = git(
        root,
        &[
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ],
    )?;
    let mut names: Vec<_> = names
        .split(|byte| *byte == 0)
        .filter(|name| !name.is_empty())
        .collect();
    names.sort_unstable();
    names.dedup();
    let mut hash = Fingerprint(0xcbf2_9ce4_8422_2325);
    for name in names {
        hash.update(&(name.len() as u64).to_le_bytes());
        hash.update(name);
        #[cfg(unix)]
        let path = {
            use std::os::unix::ffi::OsStrExt;
            root.join(std::ffi::OsStr::from_bytes(name))
        };
        #[cfg(not(unix))]
        let path = root.join(std::str::from_utf8(name).ok()?);
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                hash.update(b"deleted");
                continue;
            }
            Err(_) => return None,
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            hash.update(&metadata.permissions().mode().to_le_bytes());
        }
        if metadata.file_type().is_symlink() {
            hash.update(b"link");
            let target = std::fs::read_link(&path).ok()?;
            hash.update(target.as_os_str().as_encoded_bytes());
        } else if metadata.is_file() {
            hash.update(b"file");
            hash.update(&metadata.len().to_le_bytes());
            let mut file = std::fs::File::open(&path).ok()?;
            let mut buf = [0; 8192];
            loop {
                let count = file.read(&mut buf).ok()?;
                if count == 0 {
                    break;
                }
                hash.update(&buf[..count]);
            }
        } else {
            // Gitlinks name directories rather than ordinary source files.
            hash.update(b"directory");
        }
    }
    Some(TargetState {
        dirty: !status.is_empty(),
        hash: format!("{:016x}", hash.0),
    })
}
