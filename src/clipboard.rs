//! Política de conteúdo e envio para o clipboard.
//!
//! `choose` e `is_secret_hint` são funções puras (testáveis sem Wayland).
//! `set` delega ao `wl-copy`, que forka e mantém a seleção viva — não somos
//! servidores de seleção.

use std::io::{Read, Seek, SeekFrom, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use crate::db::{Item, Kind};

/// Tempo máximo esperando o `wl-copy` assumir a seleção.
const COPY_TIMEOUT: Duration = Duration::from_secs(2);

/// MIME que o watcher deve pedir, e o tipo de item que ele representa.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Choice {
    pub kind: Kind,
    /// Tipo principal — é o conteúdo que define o item.
    pub primary: String,
    /// Tipo secundário útil (o `text/plain` que acompanha um `text/html`).
    pub companion: Option<String>,
}

const TEXT_MIMES: [&str; 6] = [
    "text/plain;charset=utf-8",
    "text/plain;charset=UTF-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
];

const IMAGE_MIMES: [&str; 6] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/bmp",
    "image/tiff",
    "image/gif",
];

/// Escolhe o que capturar de uma oferta, na ordem fixada pela spec §7.
pub fn choose(mimes: &[String]) -> Option<Choice> {
    let has = |wanted: &str| mimes.iter().any(|mime| mime == wanted);

    if has("text/uri-list") {
        return Some(Choice {
            kind: Kind::Files,
            primary: "text/uri-list".into(),
            companion: None,
        });
    }

    if let Some(image) = IMAGE_MIMES.iter().find(|mime| has(mime)) {
        return Some(Choice {
            kind: Kind::Image,
            primary: (*image).into(),
            companion: None,
        });
    }

    if has("text/html") {
        return Some(Choice {
            kind: Kind::Html,
            primary: "text/html".into(),
            companion: TEXT_MIMES
                .iter()
                .find(|mime| has(mime))
                .map(|mime| (*mime).to_string()),
        });
    }

    TEXT_MIMES
        .iter()
        .find(|mime| has(mime))
        .map(|mime| Choice {
            kind: Kind::Text,
            primary: (*mime).to_string(),
            companion: None,
        })
}

/// Conteúdo marcado como sensível por gerenciadores de senha
/// (`x-kde-passwordManagerHint` com valor `secret`).
pub fn is_secret_hint(value: Option<&[u8]>) -> bool {
    value
        .map(|value| String::from_utf8_lossy(value).to_ascii_lowercase())
        .is_some_and(|value| value.contains("secret"))
}

/// Payload a ser colocado no clipboard.
#[derive(Debug, Clone, Copy)]
pub enum Payload<'a> {
    Text(&'a str),
    Html(&'a str),
    Image { mime: &'a str, bytes: &'a [u8] },
    Files(&'a str),
}

impl Payload<'_> {
    fn mime(&self) -> &str {
        match self {
            Payload::Text(_) => "text/plain",
            Payload::Html(_) => "text/html",
            Payload::Image { mime, .. } => mime,
            Payload::Files(_) => "text/uri-list",
        }
    }

    fn bytes(&self) -> &[u8] {
        match self {
            Payload::Text(text) | Payload::Html(text) | Payload::Files(text) => text.as_bytes(),
            Payload::Image { bytes, .. } => bytes,
        }
    }
}

/// Monta o payload de um item do histórico (mesmo caminho para a UI e a CLI).
pub fn payload_for_item(item: &Item, plain: bool) -> Result<Payload<'_>> {
    Ok(match item.kind {
        Kind::Text => Payload::Text(&item.text),
        Kind::Html => match (plain, item.html.as_deref()) {
            (false, Some(html)) => Payload::Html(html),
            _ => Payload::Text(&item.text),
        },
        Kind::Files => Payload::Files(&item.text),
        Kind::Image => {
            let bytes = item.blob.as_deref().context("imagem sem dados no banco")?;
            Payload::Image {
                mime: &item.mime,
                bytes,
            }
        }
    })
}

/// Coloca o conteúdo no clipboard via `wl-copy`.
///
/// Duas armadilhas evitadas aqui:
/// - o `wl-copy` forka para servir a seleção, então um pipe de stderr/stdout
///   herdado pelo filho nunca fecha e `output()` travaria para sempre. O stderr
///   vai para arquivo e a espera é limitada por tempo.
/// - o conteúdo vai por arquivo temporário (não por pipe), evitando EPIPE e
///   deadlock com payload grande.
pub fn set(payload: Payload<'_>) -> Result<()> {
    if which("wl-copy").is_none() {
        bail!("wl-copy não encontrado no PATH (instale o pacote wl-clipboard)");
    }

    let mut data = tempfile::tempfile().context("criando arquivo temporário")?;
    data.write_all(payload.bytes())
        .context("escrevendo conteúdo temporário")?;
    data.seek(SeekFrom::Start(0))?;

    let mut errors = tempfile::tempfile().context("criando arquivo de erro")?;
    let error_handle = errors.try_clone().context("clonando arquivo de erro")?;

    let mut child = Command::new("wl-copy")
        .arg("--type")
        .arg(payload.mime())
        .stdin(Stdio::from(data))
        .stdout(Stdio::null())
        .stderr(Stdio::from(error_handle))
        .spawn()
        .context("executando wl-copy")?;

    let deadline = Instant::now() + COPY_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().context("esperando o wl-copy")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!("wl-copy não respondeu em {COPY_TIMEOUT:?}");
        }
        std::thread::sleep(Duration::from_millis(5));
    };

    if !status.success() {
        errors.seek(SeekFrom::Start(0))?;
        let mut text = String::new();
        errors.read_to_string(&mut text).ok();
        bail!("wl-copy falhou ({status}): {}", text.trim());
    }
    Ok(())
}

pub fn which(program: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(list: &[&str]) -> Vec<String> {
        list.iter().map(|mime| (*mime).to_string()).collect()
    }

    #[test]
    fn urilist_vence_imagem_e_html() {
        let choice = choose(&mimes(&[
            "text/html",
            "image/png",
            "text/uri-list",
            "text/plain",
        ]))
        .unwrap();
        assert_eq!(choice.kind, Kind::Files);
        assert_eq!(choice.primary, "text/uri-list");
    }

    #[test]
    fn imagem_vence_html() {
        let choice = choose(&mimes(&["text/html", "text/plain", "image/png"])).unwrap();
        assert_eq!(choice.kind, Kind::Image);
        assert_eq!(choice.primary, "image/png");
    }

    #[test]
    fn html_traz_o_companion_de_texto() {
        let choice = choose(&mimes(&["text/html", "text/plain;charset=utf-8"])).unwrap();
        assert_eq!(choice.kind, Kind::Html);
        assert_eq!(choice.companion.as_deref(), Some("text/plain;charset=utf-8"));

        let without = choose(&mimes(&["text/html"])).unwrap();
        assert_eq!(without.companion, None);
    }

    #[test]
    fn texto_usa_o_mime_mais_especifico_disponivel() {
        let choice = choose(&mimes(&["UTF8_STRING", "text/plain"])).unwrap();
        assert_eq!(choice.kind, Kind::Text);
        assert_eq!(choice.primary, "text/plain");

        let choice = choose(&mimes(&["STRING", "TEXT"])).unwrap();
        assert_eq!(choice.primary, "STRING");
    }

    #[test]
    fn oferta_sem_tipo_conhecido_e_ignorada() {
        assert_eq!(
            choose(&mimes(&["chromium/x-internal-source-rfh-token", "application/xml"])),
            None
        );
    }

    #[test]
    fn hint_de_senha_veta_apenas_valor_secret() {
        assert!(is_secret_hint(Some(b"secret")));
        assert!(is_secret_hint(Some(b"Secret")));
        assert!(!is_secret_hint(Some(b"")), "valor vazio não veta");
        assert!(!is_secret_hint(None));
    }

    #[test]
    fn payload_define_mime_correto() {
        assert_eq!(Payload::Text("a").mime(), "text/plain");
        assert_eq!(Payload::Html("<b>a</b>").mime(), "text/html");
        assert_eq!(Payload::Files("file:///a").mime(), "text/uri-list");
        assert_eq!(
            Payload::Image {
                mime: "image/png",
                bytes: &[]
            }
            .mime(),
            "image/png"
        );
    }
}
