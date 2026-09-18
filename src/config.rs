//! Configuração em `~/.config/transferencia/config.toml`.
//!
//! Arquivo ausente ou inválido não é fatal: cai nos defaults e loga um aviso.
//! Chaves desconhecidas são reportadas para o usuário não achar que configurou algo.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Identificador do aplicativo (usado no single-instance, autostart e .desktop).
pub const APP_ID: &str = "com.pdl.Transferencia";

/// Lado maior das miniaturas guardadas no banco para render da lista.
pub const THUMB_MAX: u32 = 256;

/// Lado maior da prévia do painel lateral.
pub const PREVIEW_MAX: u32 = 1024;

/// Qualidade do JPEG da prévia (0-100).
pub const PREVIEW_QUALITY: u8 = 85;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Máximo de itens no histórico. Itens fixados não são removidos por isso.
    pub max_items: usize,
    /// Teto de bytes somando todos os itens.
    pub max_total_bytes: u64,
    /// Teto de bytes por item (acima disso a cópia é ignorada).
    pub max_item_bytes: u64,
    /// Teto de bytes para conteúdo textual.
    pub max_text_bytes: u64,
    /// Cola automaticamente (Ctrl+V) após escolher um item.
    pub auto_paste: bool,
    /// Espera antes de injetar o Ctrl+V, para o foco voltar ao aplicativo anterior.
    pub paste_delay_ms: u64,
    /// Largura do popup em pontos lógicos.
    pub popup_width: f32,
    /// Altura do popup em pontos lógicos.
    pub popup_height: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_items: 500,
            max_total_bytes: 256 * 1024 * 1024,
            max_item_bytes: 20 * 1024 * 1024,
            max_text_bytes: 1024 * 1024,
            auto_paste: true,
            paste_delay_ms: 150,
            popup_width: 1000.0,
            popup_height: 460.0,
        }
    }
}

const KNOWN_KEYS: [&str; 8] = [
    "max_items",
    "max_total_bytes",
    "max_item_bytes",
    "max_text_bytes",
    "auto_paste",
    "paste_delay_ms",
    "popup_width",
    "popup_height",
];

fn home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| home().join(".config"))
        .join("transferencia")
}

pub fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| home().join(".local/share"))
        .join("transferencia")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

pub fn db_path() -> PathBuf {
    data_dir().join("history.db")
}

impl Config {
    /// Carrega a configuração; nunca falha (defaults + aviso em caso de problema).
    pub fn load() -> Self {
        let path = config_path();
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        match toml::from_str::<Self>(&text) {
            Ok(cfg) => {
                if let Ok(toml::Value::Table(table)) = toml::from_str::<toml::Value>(&text) {
                    for key in table.keys() {
                        if !KNOWN_KEYS.contains(&key.as_str()) {
                            tracing::warn!("chave desconhecida em {}: {key}", path.display());
                        }
                    }
                }
                cfg
            }
            Err(err) => {
                tracing::warn!(
                    "configuração inválida em {} ({err}); usando padrões",
                    path.display()
                );
                Self::default()
            }
        }
    }

}
