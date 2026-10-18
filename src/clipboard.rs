//! Política de conteúdo e payload do clipboard.
//!
//! `choose` e `is_secret_hint` são funções puras (testáveis sem Wayland);
//! [`Payload`] é o que a seleção publica — quem a serve é [`crate::selection`].

use std::sync::Arc;

use anyhow::{Context, Result};

use crate::db::{Item, Kind};

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

/// Tipos com que servimos conteúdo textual: o `text/plain` que os alvos pedem,
/// mais os apelidos que clientes antigos ainda procuram.
const TEXT_ALIASES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "text/plain",
    "UTF8_STRING",
    "STRING",
    "TEXT",
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

    TEXT_MIMES.iter().find(|mime| has(mime)).map(|mime| Choice {
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

/// Conteúdo a publicar na seleção: cada tipo MIME com o seu próprio conteúdo.
///
/// Servir os mesmos bytes em todos os tipos — o que o `wl-copy` faz — entrega
/// marcação HTML a quem pede `text/plain`. Aqui um item HTML oferece a marcação
/// em `text/html` e o texto do companheiro nos tipos de texto.
#[derive(Debug)]
pub struct Payload {
    parts: Vec<(String, Arc<Vec<u8>>)>,
    label: String,
}

impl Payload {
    /// Monta o payload de um item do histórico (mesmo caminho para a UI e a CLI).
    pub fn for_item(item: &Item, plain: bool) -> Result<Payload> {
        let (label, parts) = match item.kind {
            Kind::Text => ("text/plain", text_parts(item.text.as_bytes())),
            Kind::Html => match (plain, item.html.as_deref()) {
                (false, Some(html)) => {
                    let mut parts =
                        vec![("text/html".to_string(), Arc::new(html.as_bytes().to_vec()))];
                    parts.extend(text_parts(item.text.as_bytes()));
                    ("text/html", parts)
                }
                _ => ("text/plain", text_parts(item.text.as_bytes())),
            },
            Kind::Files => {
                let mut parts = vec![(
                    "text/uri-list".to_string(),
                    Arc::new(item.text.as_bytes().to_vec()),
                )];
                parts.extend(text_parts(item.text.as_bytes()));
                ("text/uri-list", parts)
            }
            Kind::Image => {
                let bytes = item.blob.clone().context("imagem sem dados no banco")?;
                (
                    item.mime.as_str(),
                    vec![(item.mime.clone(), Arc::new(bytes))],
                )
            }
        };
        Ok(Payload {
            parts,
            label: label.to_string(),
        })
    }

    /// Tipo principal — para mensagens e diagnóstico.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Devolve os tipos e seus conteúdos (consumindo o payload).
    pub fn into_parts(self) -> Vec<(String, Arc<Vec<u8>>)> {
        self.parts
    }
}

/// O mesmo texto em todos os tipos que um alvo pode pedir.
fn text_parts(text: &[u8]) -> Vec<(String, Arc<Vec<u8>>)> {
    let bytes = Arc::new(text.to_vec());
    TEXT_ALIASES
        .iter()
        .map(|mime| ((*mime).to_string(), Arc::clone(&bytes)))
        .collect()
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
        assert_eq!(
            choice.companion.as_deref(),
            Some("text/plain;charset=utf-8")
        );

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
            choose(&mimes(&[
                "chromium/x-internal-source-rfh-token",
                "application/xml"
            ])),
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

    /// Item do histórico para exercitar o payload sem passar pelo banco.
    fn item(kind: Kind, mime: &str, text: &str, html: Option<&str>, blob: Option<Vec<u8>>) -> Item {
        Item {
            id: 1,
            kind,
            mime: mime.into(),
            text: text.into(),
            html: html.map(Into::into),
            blob,
            thumb: None,
            bytes: text.len() as i64,
            pinned: false,
            created_at: 0,
        }
    }

    /// O que a seleção oferece, por tipo MIME.
    fn served(payload: Payload) -> std::collections::HashMap<String, String> {
        payload
            .into_parts()
            .into_iter()
            .map(|(mime, bytes)| (mime, String::from_utf8(bytes.to_vec()).unwrap()))
            .collect()
    }

    #[test]
    fn payload_de_texto_serve_o_mesmo_conteudo_em_todos_os_tipos() {
        let payload = Payload::for_item(
            &item(Kind::Text, "text/plain;charset=utf-8", "ação", None, None),
            false,
        )
        .unwrap();
        assert_eq!(payload.label(), "text/plain");
        let served = served(payload);
        assert_eq!(served.len(), TEXT_ALIASES.len());
        for mime in TEXT_ALIASES {
            assert_eq!(served.get(mime).map(String::as_str), Some("ação"), "{mime}");
        }
    }

    #[test]
    fn payload_de_html_separa_marcacao_do_texto_puro() {
        let html = "<span>Bom dia</span>";
        let item = item(Kind::Html, "text/html", "Bom dia", Some(html), None);
        let served = served(Payload::for_item(&item, false).unwrap());
        assert_eq!(served.get("text/html").map(String::as_str), Some(html));
        for mime in TEXT_ALIASES {
            assert_eq!(
                served.get(mime).map(String::as_str),
                Some("Bom dia"),
                "alvo de texto puro recebe o companheiro, não a marcação ({mime})"
            );
        }
    }

    #[test]
    fn payload_de_html_plain_nao_oferece_marcacao() {
        let item = item(
            Kind::Html,
            "text/html",
            "Bom dia",
            Some("<span>Bom dia</span>"),
            None,
        );
        let served = served(Payload::for_item(&item, true).unwrap());
        assert!(!served.contains_key("text/html"));
        assert_eq!(
            served.get("text/plain").map(String::as_str),
            Some("Bom dia")
        );
    }

    #[test]
    fn payload_de_arquivos_oferece_uri_list_e_o_mesmo_texto() {
        let item = item(
            Kind::Files,
            "text/uri-list",
            "file:///home/pdl/nota.txt",
            None,
            None,
        );
        let served = served(Payload::for_item(&item, false).unwrap());
        for mime in ["text/uri-list", "text/plain"] {
            assert_eq!(
                served.get(mime).map(String::as_str),
                Some("file:///home/pdl/nota.txt"),
                "{mime}"
            );
        }
    }

    #[test]
    fn payload_de_imagem_nao_oferece_tipos_de_texto() {
        let item = item(Kind::Image, "image/png", "", None, Some(vec![1, 2, 3]));
        let payload = Payload::for_item(&item, false).unwrap();
        assert_eq!(payload.label(), "image/png");
        let parts = payload.into_parts();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].0, "image/png");
        assert_eq!(parts[0].1.as_slice(), [1, 2, 3]);
    }

    #[test]
    fn imagem_sem_dados_no_banco_e_erro() {
        let item = item(Kind::Image, "image/png", "", None, None);
        assert!(Payload::for_item(&item, false).is_err());
    }
}
