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
    pub smtp_dev_address: String,
    pub domain: String,
    pub tls_ca_path: Option<PathBuf>,
    pub database_path: PathBuf,
    pub identity_path: PathBuf,
    pub api_bind: String,
    pub api_tls_cert_path: PathBuf,
    pub api_tls_key_path: PathBuf,
    pub api_token_path: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        let dirs = ProjectDirs::from("dev", "Ahrara", "ahrara")
            .expect("application directories unavailable");
        Self {
            server_url: "wss://localhost:8443/ws".into(),
            smtp_dev_address: "localhost:2525".into(),
            domain: "ahrara.dev".into(),
            tls_ca_path: None,
            database_path: dirs.data_local_dir().join("ahrara.db"),
            identity_path: dirs.config_dir().join("identity.key"),
            api_bind: "127.0.0.1:8787".into(),
            api_tls_cert_path: dirs.config_dir().join("api.pem"),
            api_tls_key_path: dirs.config_dir().join("api-key.pem"),
            api_token_path: dirs.config_dir().join("api.token"),
        }
    }
}

impl Config {
    pub fn load(path: Option<&Path>) -> Result<Self> {
        let default_path = ProjectDirs::from("dev", "Ahrara", "ahrara")
            .context("application directories unavailable")?
            .config_dir()
            .join("config.toml");
        let chosen = path.unwrap_or(&default_path);
        let mut cfg = if chosen.exists() {
            toml::from_str::<Self>(&std::fs::read_to_string(chosen)?)
                .context("invalid configuration")?
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
        if !cfg.server_url.starts_with("wss://") {
            bail!("server_url must use wss://")
        }
        if cfg.domain != "ahrara.dev" {
            bail!("v1 domain must be ahrara.dev")
        }
        Ok(cfg)
    }
    pub fn profile_dir(&self) -> &Path {
        self.identity_path
            .parent()
            .expect("identity path has a parent")
    }
}

pub fn private_dir(path: &Path) -> Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
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
