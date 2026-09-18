//! Autostart do daemon via XDG (`~/.config/autostart`), que o cosmic-session
//! respeita.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::config::APP_ID;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub path: PathBuf,
    pub changed: bool,
}

pub fn autostart_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("autostart")
        .join(format!("{APP_ID}.desktop"))
}

pub fn desktop_entry(binary: &str) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=transferencia\n\
         Comment=Histórico da área de transferência (Super+V)\n\
         Exec={binary} --daemon\n\
         Terminal=false\n\
         NoDisplay=true\n\
         StartupNotify=false\n\
         X-GNOME-Autostart-enabled=true\n"
    )
}

pub fn install(binary: &str) -> Result<Outcome> {
    install_at(&autostart_path(), binary)
}

/// Instala num caminho específico (usado pelos testes).
pub fn install_at(path: &Path, binary: &str) -> Result<Outcome> {
    let desired = desktop_entry(binary);
    if let Ok(current) = std::fs::read_to_string(path)
        && current == desired
    {
        return Ok(Outcome {
            path: path.to_path_buf(),
            changed: false,
        });
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando {}", parent.display()))?;
    }
    std::fs::write(path, desired).with_context(|| format!("escrevendo {}", path.display()))?;
    Ok(Outcome {
        path: path.to_path_buf(),
        changed: true,
    })
}

pub fn uninstall() -> Result<Outcome> {
    uninstall_at(&autostart_path())
}

pub fn uninstall_at(path: &Path) -> Result<Outcome> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(Outcome {
            path: path.to_path_buf(),
            changed: true,
        }),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(Outcome {
            path: path.to_path_buf(),
            changed: false,
        }),
        Err(err) => Err(err).with_context(|| format!("removendo {}", path.display())),
    }
}

pub fn is_installed() -> bool {
    is_installed_at(&autostart_path())
}

pub fn is_installed_at(path: &Path) -> bool {
    path.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entrada_desktop_tem_exec_com_daemon() {
        let entry = desktop_entry("/usr/bin/transferencia");
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("Exec=/usr/bin/transferencia --daemon\n"));
        assert!(entry.contains("Type=Application\n"));
        assert!(entry.contains("X-GNOME-Autostart-enabled=true\n"));
        assert!(entry.ends_with('\n'));
    }

    #[test]
    fn caminho_fica_em_autostart_com_app_id() {
        let path = autostart_path();
        assert!(path.to_string_lossy().ends_with("autostart/com.pdl.Transferencia.desktop"));
    }

    #[test]
    fn instala_troca_e_remove_arquivo_de_autostart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("com.pdl.Transferencia.desktop");

        let outcome = install_at(&path, "/usr/bin/transferencia").unwrap();
        assert!(outcome.changed);
        assert!(is_installed_at(&path));
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Exec=/usr/bin/transferencia --daemon"));

        assert!(
            !install_at(&path, "/usr/bin/transferencia").unwrap().changed,
            "mesmo conteúdo não reescreve"
        );
        assert!(
            install_at(&path, "/novo/transferencia").unwrap().changed,
            "caminho novo reescreve"
        );
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("Exec=/novo/transferencia --daemon")
        );

        assert!(uninstall_at(&path).unwrap().changed);
        assert!(!is_installed_at(&path));
        assert!(!uninstall_at(&path).unwrap().changed, "remover de novo é no-op");
    }
}
