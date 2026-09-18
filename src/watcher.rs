//! Captura do clipboard via `zwlr_data_control_manager_v1`.
//!
//! Roda numa thread própria: conexão Wayland exclusiva, laço de dispatch
//! bloqueante e envio dos itens prontos por canal para a UI. Nada aqui toca o
//! banco — quem persiste é o daemon.

use std::collections::HashMap;
use std::io::Read;
use std::os::fd::AsFd;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use cosmic::cctk::sctk::reexports::protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::{self, ZwlrDataControlOfferV1},
};
use cosmic::cctk::wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use sha2::{Digest, Sha256};

use crate::clipboard;
use crate::config::{Config, PREVIEW_MAX, PREVIEW_QUALITY, THUMB_MAX};
use crate::db::{Captured, Db, Kind};

/// Gerenciadores de senha marcam conteúdo sensível com este tipo MIME.
const SECRET_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Teto de bytes lidos do hint de senha (é um valor curto, tipo `secret`).
const SECRET_HINT_LIMIT: u64 = 64;

/// Estado que a `selection` aponta: nova oferta, ou clipboard esvaziado.
enum Pending {
    Offer(ZwlrDataControlOfferV1),
    Cleared,
}

struct SeatEntry {
    seat: WlSeat,
    device: Option<ZwlrDataControlDeviceV1>,
    current: Option<ZwlrDataControlOfferV1>,
}

pub struct State {
    manager: ZwlrDataControlManagerV1,
    seats: Vec<SeatEntry>,
    /// Tipos MIME por oferta, preenchidos conforme os eventos chegam.
    offers: HashMap<ZwlrDataControlOfferV1, Vec<String>>,
    pending: Option<Pending>,
}

/// Watcher já conectado; `run` consome e só retorna em caso de erro fatal.
pub struct Watcher {
    queue: EventQueue<State>,
    state: State,
    cfg: Config,
    db: Db,
}

/// Conecta e registra os data devices. Erros aqui são imediatos e claros
/// (compositor sem o protocolo, nenhum seat, etc).
pub fn connect(cfg: Config, db: Db) -> Result<Watcher> {
    let conn = Connection::connect_to_env().context("conectando ao Wayland")?;
    let (globals, queue) = registry_queue_init::<State>(&conn)
        .map_err(|err| anyhow!("lendo os globais do Wayland: {err}"))?;
    let qh = queue.handle();

    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|err| anyhow!("compositor não oferece zwlr_data_control_manager_v1 ({err})"))?;

    let mut seats = Vec::new();
    globals.contents().with_list(|list| {
        for global in list {
            if global.interface == WlSeat::interface().name && global.version >= 2 {
                let seat: WlSeat = globals.registry().bind(global.name, 2, &qh, ());
                let device = manager.get_data_device(&seat, &qh, seat.clone());
                seats.push(SeatEntry {
                    seat,
                    device: Some(device),
                    current: None,
                });
            }
        }
    });
    if seats.is_empty() {
        bail!("nenhum seat Wayland disponível");
    }

    Ok(Watcher {
        queue,
        state: State {
            manager,
            seats,
            offers: HashMap::new(),
            pending: None,
        },
        cfg,
        db,
    })
}

/// Sobe o watcher numa thread própria. A conexão é estabelecida aqui, de forma
/// síncrona, para que falhas de setup apareçam na inicialização do daemon.
pub fn spawn(cfg: Config, db: Db) -> Result<std::thread::JoinHandle<()>> {
    let watcher = connect(cfg, db)?;
    std::thread::Builder::new()
        .name("transferencia-watcher".into())
        .spawn(move || watcher.run())
        .context("criando thread do watcher")
}

impl Watcher {
    pub fn run(mut self) {
        loop {
            if let Err(err) = self.queue.blocking_dispatch(&mut self.state) {
                tracing::error!("watcher: falha no dispatch ({err}); retomando");
                std::thread::sleep(Duration::from_millis(500));
                continue;
            }
            self.recreate_devices();
            let Some(pending) = self.state.pending.take() else {
                continue;
            };
            let offer = match pending {
                Pending::Cleared => {
                    tracing::debug!("clipboard esvaziado");
                    continue;
                }
                Pending::Offer(offer) => offer,
            };
            match self.handle_offer(&offer) {
                Ok(Some(captured)) => self.store(captured),
                Ok(None) => {}
                Err(err) => tracing::debug!("cópia ignorada: {err:#}"),
            }
        }
    }

    /// Persiste a captura; o descarte por limite roda dentro do `upsert`.
    fn store(&mut self, captured: Captured) {
        let kind = captured.kind;
        let bytes = captured.bytes.len();
        match self.db.upsert(&captured, &self.cfg) {
            Ok(upsert) => tracing::debug!(
                "capturado {kind:?} ({bytes} B) {} #{}",
                if upsert.was_inserted() {
                    "novo"
                } else {
                    "já existia, subiu para o topo"
                },
                upsert.id()
            ),
            Err(err) => tracing::warn!("gravando captura: {err:#}"),
        }
    }

    /// Recria data devices que o compositor invalidou (ex.: seat reconectado).
    fn recreate_devices(&mut self) {
        let qh = self.queue.handle();
        for entry in &mut self.state.seats {
            if entry.device.is_none() {
                tracing::info!("watcher: recriando data device");
                entry.device = Some(self.state.manager.get_data_device(&entry.seat, &qh, entry.seat.clone()));
            }
        }
    }

    /// Lê a oferta pela política da spec e devolve o item pronto (ou `None` quando
    /// não há nada que valha a pena guardar).
    fn handle_offer(&mut self, offer: &ZwlrDataControlOfferV1) -> Result<Option<Captured>> {
        // Flush: os eventos de MIME da oferta chegam depois do `selection`.
        self.queue
            .roundtrip(&mut self.state)
            .map_err(|err| anyhow!("lendo a oferta: {err}"))?;

        let mimes = self.state.offers.remove(offer).unwrap_or_default();
        if mimes.is_empty() {
            bail!("oferta sem tipos MIME");
        }

        if mimes.iter().any(|mime| mime == SECRET_HINT_MIME) {
            let value = self.read(offer, SECRET_HINT_MIME, SECRET_HINT_LIMIT)?;
            if clipboard::is_secret_hint(value.as_deref()) {
                bail!("conteúdo marcado como secreto pelo aplicativo de origem");
            }
        }

        let Some(choice) = clipboard::choose(&mimes) else {
            bail!("nenhum tipo suportado entre {mimes:?}");
        };

        let limit = match choice.kind {
            Kind::Image => self.cfg.max_item_bytes,
            _ => self.cfg.max_text_bytes,
        };
        let Some(raw) = self.read(offer, &choice.primary, limit)? else {
            bail!("conteúdo vazio");
        };

        let (text, html, bytes, thumb, preview) = match choice.kind {
            Kind::Text | Kind::Files => {
                let text = String::from_utf8_lossy(&raw).into_owned();
                (text, None, raw, None, None)
            }
            Kind::Html => {
                let html = String::from_utf8_lossy(&raw).into_owned();
                let companion = match choice.companion.as_deref() {
                    Some(mime) => self
                        .read(offer, mime, self.cfg.max_text_bytes)?
                        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                    None => None,
                };
                let text = companion.unwrap_or_else(|| strip_html(&html));
                (text, Some(html), raw, None, None)
            }
            Kind::Image => {
                let thumb = thumbnail(&raw);
                let preview = preview_image(&raw);
                (String::new(), None, raw, thumb, preview)
            }
        };

        let mut hasher = Sha256::new();
        hasher.update(choice.kind.as_str().as_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
        let sha256 = format!("{:x}", hasher.finalize());

        Ok(Some(Captured {
            kind: choice.kind,
            mime: choice.primary,
            text,
            html,
            bytes,
            thumb,
            preview,
            sha256,
        }))
    }

    /// Pede um tipo MIME da oferta e lê o conteúdo por pipe, com teto de bytes.
    fn read(
        &mut self,
        offer: &ZwlrDataControlOfferV1,
        mime: &str,
        limit: u64,
    ) -> Result<Option<Vec<u8>>> {
        let (mut reader, writer) = std::io::pipe().context("criando pipe de transferência")?;
        offer.receive(mime.to_string(), writer.as_fd());
        drop(writer);
        self.queue
            .roundtrip(&mut self.state)
            .map_err(|err| anyhow!("solicitando {mime}: {err}"))?;

        let mut buffer = Vec::new();
        reader
            .by_ref()
            .take(limit + 1)
            .read_to_end(&mut buffer)
            .with_context(|| format!("lendo {mime}"))?;
        if buffer.len() as u64 > limit {
            bail!("conteúdo de {mime} acima do limite de {limit} bytes");
        }
        if buffer.is_empty() {
            return Ok(None);
        }
        Ok(Some(buffer))
    }
}

/// Gera a miniatura PNG usada na lista. Falha de decodificação não impede a
/// captura: o item fica sem miniatura.
fn thumbnail(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?;
    let thumb = image.thumbnail(THUMB_MAX, THUMB_MAX);
    let mut out = Vec::new();
    thumb
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

/// Gera a prévia maior do painel lateral. JPEG para fotos/prints (bem menor que
/// PNG); PNG quando a imagem tem transparência, para não achatá-la em preto.
fn preview_image(bytes: &[u8]) -> Option<Vec<u8>> {
    let image = image::load_from_memory(bytes).ok()?;
    let preview = image.thumbnail(PREVIEW_MAX, PREVIEW_MAX);
    let mut out = Vec::new();
    if preview.color().has_alpha() {
        preview
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .ok()?;
    } else {
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(
            std::io::Cursor::new(&mut out),
            PREVIEW_QUALITY,
        );
        encoder.encode_image(&preview.to_rgb8()).ok()?;
    }
    if out.is_empty() { None } else { Some(out) }
}

/// Completa a prévia de imagens capturadas antes do painel de prévia existir.
/// Roda numa thread própria e não bloqueia nada.
pub fn backfill_previews(mut db: Db) {
    std::thread::Builder::new()
        .name("transferencia-preview".into())
        .spawn(move || {
            let ids = match db.images_without_preview(500) {
                Ok(ids) => ids,
                Err(err) => {
                    tracing::warn!("procurando prévias antigas: {err:#}");
                    return;
                }
            };
            if ids.is_empty() {
                return;
            }
            let mut done = 0;
            for id in ids {
                let Ok(Some(blob)) = db.blob_of(id) else {
                    continue;
                };
                if let Some(preview) = preview_image(&blob)
                    && db.set_preview(id, &preview).unwrap_or(false)
                {
                    done += 1;
                }
            }
            tracing::info!("prévias geradas para {done} imagem(ns) antiga(s)");
        })
        .ok();
}

/// Texto aproximado de um HTML, para busca e prévia quando não vem o
/// `text/plain` companion.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            '&' if !in_tag => {
                match entity_at(&mut chars) {
                    Some(text) => out.push_str(&text),
                    None => out.push('&'),
                }
            }
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out
}

/// Lê uma entidade logo após o `&`, consumindo-a se for reconhecida.
fn entity_at(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<String> {
    let mut name = String::new();
    let mut lookahead = chars.clone();
    let mut terminated = false;
    while let Some(next) = lookahead.next() {
        if next == ';' {
            terminated = true;
            break;
        }
        if name.len() >= 10 || !(next.is_ascii_alphanumeric() || next == '#') {
            break;
        }
        name.push(next);
    }
    if !terminated {
        return None;
    }
    let decoded = decode_entity(&name)?;
    for _ in 0..=name.chars().count() {
        chars.next();
    }
    Some(decoded)
}

fn decode_entity(name: &str) -> Option<String> {
    match name {
        "amp" => Some("&".to_string()),
        "lt" => Some("<".to_string()),
        "gt" => Some(">".to_string()),
        "quot" => Some("\"".to_string()),
        "apos" => Some("'".to_string()),
        "nbsp" => Some(" ".to_string()),
        other => {
            let digits = other.strip_prefix('#')?;
            let (radix, digits) = match digits.strip_prefix(['x', 'X']) {
                Some(hex) => (16, hex),
                None => (10, digits),
            };
            let code = u32::from_str_radix(digits, radix).ok()?;
            char::from_u32(code).map(String::from)
        }
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlRegistry,
        _event: <WlRegistry as Proxy>::Event,
        _data: &GlobalListContents,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlManagerV1,
        _event: <ZwlrDataControlManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &WlSeat,
        _event: <WlSeat as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlDeviceV1, WlSeat> for State {
    fn event(
        state: &mut Self,
        device: &ZwlrDataControlDeviceV1,
        event: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        _seat: &WlSeat,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_device_v1::Event::DataOffer { id } => {
                state.offers.insert(id, Vec::new());
            }
            zwlr_data_control_device_v1::Event::Selection { id } => {
                let previous = state
                    .seats
                    .iter_mut()
                    .find(|entry| entry.device.as_ref() == Some(device))
                    .and_then(|entry| std::mem::replace(&mut entry.current, id.clone()));
                if let Some(previous) = previous {
                    state.offers.remove(&previous);
                }
                state.pending = Some(match id {
                    Some(offer) => Pending::Offer(offer),
                    None => Pending::Cleared,
                });
            }
            zwlr_data_control_device_v1::Event::Finished => {
                tracing::info!("watcher: data device encerrado pelo compositor");
                if let Some(entry) = state
                    .seats
                    .iter_mut()
                    .find(|entry| entry.device.as_ref() == Some(device))
                {
                    // Não chamamos `destroy`: quem destruiu foi o compositor.
                    entry.device = None;
                }
            }
            _ => {}
        }
    }

    // Precisa ficar DENTRO deste impl: a macro gera o método `event_created_child`
    // que o despachante usa para dar user-data ao `zwlr_data_control_offer_v1`
    // criado pelo evento `data_offer`.
    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for State {
    fn event(
        state: &mut Self,
        offer: &ZwlrDataControlOfferV1,
        event: <ZwlrDataControlOfferV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        if let zwlr_data_control_offer_v1::Event::Offer { mime_type } = event
            && let Some(mimes) = state.offers.get_mut(offer)
            && !mimes.contains(&mime_type)
        {
            mimes.push(mime_type);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_html_remove_tags_e_decodifica_entidades() {
        assert_eq!(
            strip_html("<p>Olá <b>mundo</b> &amp; cia</p>"),
            "Olá mundo & cia"
        );
        assert_eq!(strip_html("a<br/>b"), "ab");
        assert_eq!(strip_html("&desconhecida;"), "&desconhecida;");
        assert_eq!(strip_html("2 &lt; 3"), "2 < 3");
        assert_eq!(strip_html("&#39;aspas&#x27;"), "'aspas'");
        assert_eq!(strip_html("100% & pico"), "100% & pico");
        assert_eq!(strip_html("a & b"), "a & b");
    }

    #[test]
    fn thumbnail_reduz_e_gera_png_valido() {
        let mut source = image::RgbaImage::new(1024, 512);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgba([10, 200, 30, 255]);
        }
        let mut encoded = Vec::new();
        source
            .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
            .unwrap();

        let thumb = thumbnail(&encoded).expect("gera miniatura");
        let decoded = image::load_from_memory(&thumb).expect("miniatura é PNG válido");
        assert!(decoded.width() <= THUMB_MAX && decoded.height() <= THUMB_MAX);
        assert!(decoded.width() > 0 && decoded.height() > 0);
    }

    #[test]
    fn thumbnail_de_dados_invalidos_e_none() {
        assert!(thumbnail(b"nao e imagem").is_none());
        assert!(preview_image(b"nao e imagem").is_none());
    }

    #[test]
    fn preview_reduz_para_o_tamanho_maximo_e_gera_imagem_valida() {
        let mut source = image::RgbImage::new(2400, 1200);
        for pixel in source.pixels_mut() {
            *pixel = image::Rgb([200, 40, 90]);
        }
        let mut encoded = Vec::new();
        source
            .write_to(&mut std::io::Cursor::new(&mut encoded), image::ImageFormat::Png)
            .unwrap();

        let preview = preview_image(&encoded).expect("gera prévia");
        let decoded = image::load_from_memory(&preview).expect("prévia decodifica");
        assert!(decoded.width() <= PREVIEW_MAX && decoded.height() <= PREVIEW_MAX);
        assert_eq!(decoded.width(), PREVIEW_MAX);
        assert!(
            preview.len() < encoded.len(),
            "prévia JPEG menor que o PNG original ({} vs {})",
            preview.len(),
            encoded.len()
        );
    }
}
