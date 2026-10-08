use anyhow::{Context, Result, bail};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub server_url: String,
    /// Public Noise key provisioned independently of the relay connection.
    pub server_public_key: Option<String>,
    pub tls_ca_path: Option<PathBuf>,
    pub database_path: PathBuf,
    pub identity_path: PathBuf,
    pub api_bind: String,
    pub api_https: bool,
    pub api_tls_cert_path: PathBuf,
    pub api_tls_key_path: PathBuf,
    pub api_token_path: PathBuf,
    pub mcp_auth: bool,
    pub check_for_updates: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self::defaults(ProjectDirs::from("dev", "Ahrara", "ahrara").as_ref())
    }
}

impl Config {
    // Serde also uses these defaults for an explicit --config. Without a home
    // directory, `load` resolves the relative paths beside that file instead.
    pub(crate) fn defaults(dirs: Option<&ProjectDirs>) -> Self {
        let data = dirs.map_or_else(PathBuf::new, |dirs| dirs.data_local_dir().to_owned());
        let config = dirs.map_or_else(PathBuf::new, |dirs| dirs.config_dir().to_owned());
        Self {
            server_url: "wss://relay.ahrara.dev".into(),
            server_public_key: option_env!("AHRARA_SERVER_PUBLIC_KEY")
                .filter(|key| !key.is_empty())
                .map(str::to_owned),
            tls_ca_path: None,
            database_path: data.join("ahrara.db"),
            identity_path: config.join("identity.key"),
            api_bind: "127.0.0.1:8787".into(),
            api_https: false,
            api_tls_cert_path: config.join("api.pem"),
            api_tls_key_path: config.join("api-key.pem"),
            api_token_path: config.join("api.token"),
            mcp_auth: true,
            check_for_updates: true,
        }
    }

    pub fn load(path: Option<&Path>) -> Result<Self> {
        let default_path = ProjectDirs::from("dev", "Ahrara", "ahrara")
            .context("application directories unavailable")?
            .config_dir()
            .join("config.toml");
        let chosen = path.unwrap_or(&default_path);
        let mut cfg = if chosen.exists() {
            let mut table: toml::Table = toml::from_str(&std::fs::read_to_string(chosen)?)
                .context("invalid configuration")?;
            // Retired keys remain in older and development profiles.
            table.remove("domain");
            table.remove("smtp_dev_address");
            table.try_into::<Self>().context("invalid configuration")?
        } else if path.is_some() {
            bail!("configuration file does not exist: {}", chosen.display())
        } else {
            Self::default()
        };
        let base = std::path::absolute(chosen)?
            .parent()
            .context("invalid config path")?
            .to_owned();

        for p in [
            &mut cfg.database_path,
            &mut cfg.identity_path,
            &mut cfg.api_token_path,
            &mut cfg.api_tls_cert_path,
            &mut cfg.api_tls_key_path,
        ] {
            if p.is_relative() {
                *p = base.join(&*p);
            }
        }
        if let Some(p) = &mut cfg.tls_ca_path
            && p.is_relative()
        {
            *p = base.join(&*p);
        }

        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        let server = url::Url::parse(&self.server_url).context("invalid server_url")?;
        if let Some(key) = &self.server_public_key {
            crate::secure::parse_public_key(key)?;
        }
        if server.scheme() != "wss" {
            bail!("server_url must use wss://")
        }
        if !server.username().is_empty()
            || server.password().is_some()
            || server.fragment().is_some()
        {
            bail!("server_url must not contain credentials or a fragment")
        }

        Ok(())
    }

    pub fn profile_dir(&self) -> &Path {
        self.identity_path
            .parent()
            .expect("identity path has a parent")
    }
}

/// Creates an owner-only directory. Existing directories, such as a caller's
/// chosen profile location, keep their permissions.
pub fn private_dir(path: &Path) -> Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }

    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    match builder.create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    #[cfg(windows)]
    private_windows_acl(path)?;
    Ok(())
}

pub fn private_write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<()> {
    let parent = path.parent().context("file needs parent directory")?;
    private_dir(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        temp.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    private_windows_acl(temp.path())?;
    temp.write_all(bytes)?;
    temp.as_file().sync_all()?;
    if overwrite {
        temp.persist(path).map_err(|e| e.error)?;
    } else {
        temp.persist_noclobber(path).map_err(|e| e.error)?;
    }

    Ok(())
}

// Owner-only protected DACL; newly created children inherit owner-only access.
// https://learn.microsoft.com/en-us/windows/win32/secauthz/sid-strings
#[cfg(windows)]
fn private_windows_acl(path: &Path) -> Result<()> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION, SetFileSecurityW,
        },
    };
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let sddl: Vec<u16> = "D:P(A;OICI;FA;;;OW)"
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut descriptor = ptr::null_mut();
    // SAFETY: UTF-16 strings are NUL terminated; the descriptor is allocated by
    // Windows, used only until SetFileSecurityW returns, and always LocalFree'd.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error().into());
        }

        let result = SetFileSecurityW(
            path.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        let error = if result == 0 {
            Some(std::io::Error::last_os_error())
        } else {
            None
        };
        LocalFree(descriptor);
        if let Some(error) = error {
            return Err(error.into());
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn private_dir_hardens_only_directories_it_creates() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        let root = tempfile::tempdir().unwrap();
        let existing = root.path().join("shared");
        std::fs::create_dir(&existing).unwrap();
        std::fs::set_permissions(&existing, std::fs::Permissions::from_mode(0o755)).unwrap();
        private_dir(&existing).unwrap();
        let created = existing.join("profile");
        private_dir(&created).unwrap();

        assert_eq!(mode(&existing), 0o755);
        assert_eq!(mode(&created), 0o700);
    }

    #[test]
    fn defaults_without_application_directories_stay_relative_to_the_config() {
        let config = Config::defaults(None);

        assert_eq!(config.database_path, Path::new("ahrara.db"));
        assert_eq!(config.identity_path, Path::new("identity.key"));
        assert_eq!(config.api_token_path, Path::new("api.token"));
    }

    #[test]
    fn retired_keys_load_but_unknown_keys_fail() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(
            &path,
            "domain = \"ahrara.dev\"\nsmtp_dev_address = \"localhost:2525\"\n",
        )
        .unwrap();

        assert!(Config::load(Some(&path)).is_ok());
        std::fs::write(&path, "unknown = 1\n").unwrap();

        assert!(Config::load(Some(&path)).is_err());
    }
}
