//! Private, opt-in credentials for a fixed loopback endpoint.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

pub(super) const LOCAL_PORT: u16 = 38761;
const TOKEN_LEN: usize = 64;

pub(super) struct SavedPairing {
    file: Option<PathBuf>,
}

impl SavedPairing {
    pub fn new(internal: Option<&Path>) -> Self {
        Self { file: internal.map(|dir| dir.join("mcp-local-pairing-v1")) }
    }

    pub fn available(&self) -> bool {
        self.file.is_some()
    }

    pub fn exists(&self) -> bool {
        self.file.as_ref().is_some_and(|path| path.is_file())
    }

    pub fn token(&self, create: impl FnOnce() -> io::Result<String>) -> io::Result<String> {
        let path = self.file.as_ref().ok_or_else(|| io::Error::other("Android private storage is unavailable."))?;
        match File::open(path) {
            Ok(file) => {
                let mut token = String::new();
                file.take((TOKEN_LEN + 1) as u64).read_to_string(&mut token)?;
                if token.len() == TOKEN_LEN && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                    return Ok(token);
                }
                Err(io::Error::new(io::ErrorKind::InvalidData, "Forget the saved pairing, then create it again."))
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let token = create()?;
                let mut file = OpenOptions::new().write(true).create_new(true).mode(0o600).open(path)?;
                if let Err(error) = file.write_all(token.as_bytes()).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(path);
                    return Err(error);
                }
                Ok(token)
            }
            Err(error) => Err(error),
        }
    }

    pub fn forget(&self) -> io::Result<()> {
        if let Some(path) = &self.file
            && let Err(error) = fs::remove_file(path)
            && error.kind() != io::ErrorKind::NotFound
        {
            return Err(error);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn reuse_private_token_until_revoked() {
        let folder = std::env::temp_dir().join(format!("filmcraft-local-pairing-test-{}", std::process::id()));
        fs::create_dir(&folder).unwrap();
        let pairing = SavedPairing::new(Some(&folder));
        let first = pairing.token(|| Ok("a".repeat(TOKEN_LEN))).unwrap();
        let second = pairing.token(|| panic!("saved token should be used")).unwrap();
        assert_eq!(first, second);
        assert_eq!(fs::metadata(folder.join("mcp-local-pairing-v1")).unwrap().permissions().mode() & 0o777, 0o600);
        pairing.forget().unwrap();
        assert_ne!(first, pairing.token(|| Ok("b".repeat(TOKEN_LEN))).unwrap());
        pairing.forget().unwrap();
        fs::remove_dir(folder).unwrap();
    }
}
