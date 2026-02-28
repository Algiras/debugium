use std::path::PathBuf;
use anyhow::Result;

/// Manages the `~/.debugium/` home directory for Debugium state.
pub struct DebugiumHome {
    pub path: PathBuf,
}

impl DebugiumHome {
    /// Open (and create if absent) the `~/.debugium/` directory.
    pub fn open() -> Result<Self> {
        let home_dir = dirs::home_dir()
            .ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
        let path = home_dir.join(".debugium");
        std::fs::create_dir_all(&path)?;
        Ok(Self { path })
    }

    /// Write the server port to `~/.debugium/port` atomically.
    pub fn write_port(&self, port: u16) {
        let port_path = self.path.join("port");
        let tmp_path = self.path.join("port.tmp");
        if let Err(e) = std::fs::write(&tmp_path, format!("{}\n", port)) {
            tracing::warn!("Failed to write port tmp file: {e}");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &port_path) {
            tracing::warn!("Failed to rename port file: {e}");
        }
    }

    /// Delete `~/.debugium/port` on clean shutdown.
    pub fn remove_port(&self) {
        let port_path = self.path.join("port");
        let _ = std::fs::remove_file(&port_path);
    }

    /// Path to the log file: `~/.debugium/debugium.log`.
    pub fn log_path(&self) -> PathBuf {
        self.path.join("debugium.log")
    }

    /// Path to a session directory: `~/.debugium/sessions/<id>/`.
    pub fn session_dir(&self, id: &str) -> PathBuf {
        self.path.join("sessions").join(id)
    }

    /// Ensure `~/.debugium/sessions/<id>/` exists and return the path.
    pub fn ensure_session_dir(&self, id: &str) -> Result<PathBuf> {
        let dir = self.session_dir(id);
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }
}
