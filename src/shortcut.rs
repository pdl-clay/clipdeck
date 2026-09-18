//! Instalação do atalho Super+V no arquivo de atalhos do COSMIC.
//!
//! O `cosmic-config` guarda `~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom`
//! num formato RON com chaves que são tuplas — texto que não dá para gerar por
//! serialização sem replicar os tipos do COSMIC. Então editamos o texto:
//! backup antes de escrever, inserção idempotente e remoção cirúrgica da nossa
//! entrada. O arquivo é observado por inotify pelo cosmic-config, que recarrega
//! sozinho.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::config::{APP_ID, config_dir};

/// Texto de descrição que identifica a nossa entrada no arquivo.
pub const DESCRIPTION: &str = "histórico da área de transferência";
/// Argumento que a entrada invoca.
const TOGGLE_ARG: &str = "--toggle";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub path: PathBuf,
    pub changed: bool,
    pub backup: Option<PathBuf>,
}

/// Caminho do arquivo de atalhos customizados do COSMIC.
pub fn shortcut_path() -> PathBuf {
    cosmic_dir()
        .join("com.system76.CosmicSettings.Shortcuts")
        .join("v1")
        .join("custom")
}

fn cosmic_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| config_dir().parent().unwrap_or(&config_dir()).to_path_buf())
        .join("cosmic")
}

/// Entrada gerada, no mesmo estilo do que o cosmic-settings escreve.
fn entry(binary: &str) -> String {
    format!(
        "    (\n        modifiers: [\n            Super,\n        ],\n        key: \"v\",\n        description: Some(\"{DESCRIPTION}\"),\n    ): Spawn(\"{binary} {TOGGLE_ARG}\"),\n"
    )
}

fn is_our_entry(unit: &str, binary: &str) -> bool {
    unit.contains(DESCRIPTION)
        || (unit.contains(TOGGLE_ARG) && (unit.contains(binary) || unit.contains("transferencia")))
}

/// Divide o conteúdo em três partes: prefixo (cabeçalho), entradas de nível 0 e
/// sufixo (fechamento e sobras).
///
/// Uma entrada começa numa linha de nível 0 que abre a chave com `(` e termina na
/// primeira linha seguinte em que a profundidade volta a zero fechando com `),`.
/// Parênteses e colchetes dentro de strings não contam, então descrições com
/// parênteses não quebram a divisão.
fn split_entries(content: &str) -> (Vec<String>, String, String) {
    let mut entries = Vec::new();
    let mut prefix = String::new();
    let mut suffix = String::new();
    let mut current = String::new();
    let mut depth: i32 = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut in_entry = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if in_entry {
            current.push_str(line);
            current.push('\n');
        } else if depth == 0 && trimmed.starts_with('(') {
            in_entry = true;
            current.push_str(line);
            current.push('\n');
        } else if entries.is_empty() {
            prefix.push_str(line);
            prefix.push('\n');
        } else {
            suffix.push_str(line);
            suffix.push('\n');
        }

        for ch in line.chars() {
            if in_string {
                if escaped {
                    escaped = false;
                } else if ch == '\\' {
                    escaped = true;
                } else if ch == '"' {
                    in_string = false;
                }
                continue;
            }
            match ch {
                '"' => in_string = true,
                // Só parênteses/colchetes contam: as chaves externas são do mapa.
                '(' | '[' => depth += 1,
                ')' | ']' => depth -= 1,
                _ => {}
            }
        }

        if in_entry && depth == 0 && trimmed.ends_with("),") {
            entries.push(std::mem::take(&mut current));
            in_entry = false;
        }
    }

    if !current.is_empty() {
        // Entrada sem fechamento reconhecido: preserva no fim em vez de perder.
        suffix.push_str(&current);
    }
    (entries, prefix, suffix)
}

fn build(entries: &[String], prefix: &str, suffix: &str) -> String {
    let mut out = String::new();
    if prefix.trim().is_empty() {
        out.push_str("{\n");
    } else {
        out.push_str(prefix);
    }
    for entry in entries {
        out.push_str(entry);
    }
    out.push_str(suffix);
    if !out.ends_with('}') && !out.ends_with("}\n") {
        out.push_str("}\n");
    }
    out
}

/// Verificação estrutural mínima: chaves balanceadas e o arquivo fechando.
fn looks_well_formed(content: &str) -> bool {
    let opened = content.matches('{').count();
    let closed = content.matches('}').count();
    opened == closed && opened > 0
}

pub fn install(binary: &str) -> Result<Outcome> {
    install_at(&shortcut_path(), binary)
}

/// Instala num arquivo específico (usado pelos testes).
pub fn install_at(path: &Path, binary: &str) -> Result<Outcome> {
    let path = path.to_path_buf();
    let original = read_or_empty(&path)?;
    let (entries, prefix, suffix) = split_entries(&original);
    let wanted = format!("Spawn(\"{binary} {TOGGLE_ARG}\")");

    if entries
        .iter()
        .any(|entry| is_our_entry(entry, binary) && entry.contains(&wanted))
    {
        return Ok(Outcome {
            path,
            changed: false,
            backup: None,
        });
    }

    // Entradas nossas apontando para outro caminho são substituídas, não somadas.
    let mut kept: Vec<String> = entries
        .iter()
        .filter(|entry| !is_our_entry(entry, binary))
        .cloned()
        .collect();
    kept.push(entry(binary));

    let updated = build(&kept, &prefix, &suffix);
    if !looks_well_formed(&updated) {
        bail!(
            "recusando escrever {}: resultado ficou malformado",
            path.display()
        );
    }
    let backup = write_with_backup(&path, &original, &updated)?;
    Ok(Outcome {
        path,
        changed: true,
        backup,
    })
}

pub fn uninstall(binary: &str) -> Result<Outcome> {
    uninstall_at(&shortcut_path(), binary)
}

/// Remove de um arquivo específico (usado pelos testes).
pub fn uninstall_at(path: &Path, binary: &str) -> Result<Outcome> {
    let path = path.to_path_buf();
    let original = read_or_empty(&path)?;
    let (entries, prefix, suffix) = split_entries(&original);
    let kept: Vec<String> = entries
        .iter()
        .filter(|entry| !is_our_entry(entry, binary))
        .cloned()
        .collect();

    if kept.len() == entries.len() {
        return Ok(Outcome {
            path,
            changed: false,
            backup: None,
        });
    }

    let updated = build(&kept, &prefix, &suffix);
    if !looks_well_formed(&updated) {
        bail!(
            "recusando escrever {}: resultado ficou malformado",
            path.display()
        );
    }
    let backup = write_with_backup(&path, &original, &updated)?;
    Ok(Outcome {
        path,
        changed: true,
        backup,
    })
}

/// Informa se o atalho já está instalado (para o `doctor`).
pub fn is_installed(binary: &str) -> bool {
    is_installed_at(&shortcut_path(), binary)
}

pub fn is_installed_at(path: &Path, binary: &str) -> bool {
    match read_or_empty(path) {
        Ok(content) => {
            let (entries, _, _) = split_entries(&content);
            entries
                .iter()
                .any(|entry| is_our_entry(entry, binary) && entry.contains(TOGGLE_ARG))
        }
        Err(_) => false,
    }
}

fn read_or_empty(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(err) => Err(err).with_context(|| format!("lendo {}", path.display())),
    }
}

fn write_with_backup(path: &Path, original: &str, updated: &str) -> Result<Option<PathBuf>> {
    let backup = if original.trim().is_empty() {
        None
    } else {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let backup = path.with_extension(format!("bak-{stamp}"));
        std::fs::write(&backup, original)
            .with_context(|| format!("criando backup {}", backup.display()))?;
        Some(backup)
    };

    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("criando {}", parent.display()))?;
    }
    // Escrita atômica: o cosmic-config observa o diretório.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, updated).with_context(|| format!("escrevendo {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("movendo para {}", path.display()))?;
    Ok(backup)
}

/// Caminho do executável atual, para gravar no atalho.
pub fn current_binary() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.canonicalize().ok())
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_else(|| APP_ID.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMOSTRA: &str = r#"{
    (
        modifiers: [
            Alt,
        ],
        key: "2",
    ): Workspace(2),
    (
        modifiers: [
            Super,
        ],
        key: "Return",
        description: Some("terminal"),
    ): Spawn("alacritty"),
}
"#;

    #[test]
    fn insere_entrada_preservando_as_existentes() {
        let (mut entries, prefix, suffix) = split_entries(AMOSTRA);
        assert_eq!(entries.len(), 2);
        entries.push(entry("/usr/bin/transferencia"));
        let updated = build(&entries, &prefix, &suffix);

        assert!(looks_well_formed(&updated));
        assert!(updated.contains("Spawn(\"alacritty\")"));
        assert!(updated.contains("Workspace(2)"));
        assert!(updated.contains("key: \"v\""));
        assert!(updated.contains("Spawn(\"/usr/bin/transferencia --toggle\")"));
        assert_eq!(updated.matches(DESCRIPTION).count(), 1);
        // a entrada nova entra depois das existentes
        let alacritty = updated.find("alacritty").unwrap();
        let ours = updated.find(DESCRIPTION).unwrap();
        assert!(ours > alacritty);
    }

    #[test]
    fn arquivo_vazio_gera_mapa_valido() {
        let (entries, prefix, suffix) = split_entries("");
        let updated = build(&entries, &prefix, &suffix);
        assert!(looks_well_formed(&updated));
        assert_eq!(updated.trim(), "{\n}");
    }

    #[test]
    fn instalacao_e_idempotente() {
        let (mut entries, prefix, suffix) = split_entries(AMOSTRA);
        entries.push(entry("/usr/bin/transferencia"));
        let once = build(&entries, &prefix, &suffix);

        let (entries_again, prefix_again, suffix_again) = split_entries(&once);
        let ours = entries_again
            .iter()
            .filter(|e| is_our_entry(e, "/usr/bin/transferencia"))
            .count();
        assert_eq!(ours, 1, "entrada duplicaria na segunda chamada");
        let twice = build(&entries_again, &prefix_again, &suffix_again);
        assert_eq!(once, twice, "conteúdo estável entre chamadas");
    }

    #[test]
    fn remocao_restaura_o_conteudo_original() {
        let (mut entries, prefix, suffix) = split_entries(AMOSTRA);
        entries.push(entry("/usr/bin/transferencia"));
        let with_entry = build(&entries, &prefix, &suffix);

        let (entries, prefix, suffix) = split_entries(&with_entry);
        let kept: Vec<String> = entries
            .iter()
            .filter(|e| !is_our_entry(e, "/usr/bin/transferencia"))
            .cloned()
            .collect();
        let restored = build(&kept, &prefix, &suffix);
        assert_eq!(restored, AMOSTRA, "volta byte a byte ao original");
    }

    #[test]
    fn entrada_sem_fechamento_nao_e_perdida() {
        let truncated = "{\n    (\n        modifiers: [\n            Super,\n";
        let (entries, _prefix, suffix) = split_entries(truncated);
        assert!(entries.is_empty());
        assert!(suffix.contains("modifiers"));
    }

    #[test]
    fn instalacao_troca_o_caminho_do_binario() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom");
        std::fs::write(&path, AMOSTRA).unwrap();

        assert!(install_at(&path, "/antigo/transferencia").unwrap().changed);
        assert!(is_installed_at(&path, "/antigo/transferencia"));

        assert!(
            install_at(&path, "/usr/bin/transferencia").unwrap().changed,
            "troca de caminho precisa reescrever o atalho"
        );
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Spawn(\"/usr/bin/transferencia --toggle\")"));
        assert!(!content.contains("/antigo/transferencia"));
        assert_eq!(content.matches(DESCRIPTION).count(), 1);
        assert!(content.contains("Spawn(\"alacritty\")"), "antigas intactas");

        assert!(
            !install_at(&path, "/usr/bin/transferencia").unwrap().changed,
            "instalação repetida não muda nada"
        );

        assert!(uninstall_at(&path, "/usr/bin/transferencia").unwrap().changed);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            AMOSTRA,
            "desinstalar devolve o arquivo original"
        );
        assert!(!uninstall_at(&path, "/usr/bin/transferencia").unwrap().changed);
    }

    #[test]
    fn arquivo_ausente_e_criado_e_depois_limpo() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("custom");
        assert!(!is_installed_at(&path, "/usr/bin/transferencia"));

        let outcome = install_at(&path, "/usr/bin/transferencia").unwrap();
        assert!(outcome.changed);
        assert!(outcome.backup.is_none(), "sem arquivo original, sem backup");
        assert!(is_installed_at(&path, "/usr/bin/transferencia"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(looks_well_formed(&content));
        assert!(content.contains("key: \"v\""));

        assert!(uninstall_at(&path, "/usr/bin/transferencia").unwrap().changed);
        assert_eq!(std::fs::read_to_string(&path).unwrap().trim(), "{\n}");
    }
}
