//! Publicação da seleção via `zwlr_data_control_manager_v1`.
//!
//! O `wl-copy` anuncia vários tipos MIME servindo os **mesmos bytes** em todos
//! eles: um item HTML chegava aos alvos de texto puro como marcação. Aqui cada
//! tipo tem o seu conteúdo ([`Payload`]), e quem serve é este processo — no
//! daemon, numa thread; na CLI, num processo filho de `copy`, porque a seleção
//! morre junto com quem a publica.

use std::io::Write;
use std::os::fd::OwnedFd;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use cosmic::cctk::sctk::reexports::protocols_wlr::data_control::v1::client::{
    zwlr_data_control_device_v1::{self, ZwlrDataControlDeviceV1},
    zwlr_data_control_manager_v1::ZwlrDataControlManagerV1,
    zwlr_data_control_offer_v1::ZwlrDataControlOfferV1,
    zwlr_data_control_source_v1::{self, ZwlrDataControlSourceV1},
};
use cosmic::cctk::wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle, event_created_child,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};

use crate::clipboard::Payload;

/// Estado do dispatch: o que oferecemos e se outro cliente assumiu a seleção.
struct State {
    parts: Arc<Vec<(String, Arc<Vec<u8>>)>>,
    cancelled: bool,
}

/// Seleção publicada: enquanto este valor existir, o conteúdo é o do clipboard.
pub struct Server {
    queue: EventQueue<State>,
    state: State,
    // Os proxies ficam vivos até o fim de [`Server::run`]: destruí-los (o que o
    // drop faz) destruiria a seleção antes de os alvos lerem.
    _source: ZwlrDataControlSourceV1,
    _manager: ZwlrDataControlManagerV1,
    _devices: Vec<ZwlrDataControlDeviceV1>,
}

impl Server {
    /// Publica o payload e só retorna quando o compositor já o tem como seleção.
    pub fn start(payload: Payload) -> Result<Server> {
        let conn = Connection::connect_to_env().context("conectando ao Wayland")?;
        let (globals, mut queue) = registry_queue_init::<State>(&conn)
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
                    seats.push(seat);
                }
            }
        });
        if seats.is_empty() {
            bail!("nenhum seat Wayland disponível");
        }

        let parts = Arc::new(payload.into_parts());
        let source = manager.create_data_source(&qh, ());
        for (mime, _) in parts.iter() {
            source.offer(mime.clone());
        }
        let devices: Vec<ZwlrDataControlDeviceV1> = seats
            .iter()
            .map(|seat| {
                let device = manager.get_data_device(seat, &qh, ());
                device.set_selection(Some(&source));
                device
            })
            .collect();

        let mut state = State {
            parts,
            cancelled: false,
        };
        queue
            .roundtrip(&mut state)
            .map_err(|err| anyhow!("publicando a seleção: {err}"))?;
        Ok(Server {
            queue,
            state,
            _source: source,
            _manager: manager,
            _devices: devices,
        })
    }

    /// Atende os pedidos dos alvos até outro cliente assumir a seleção.
    pub fn run(mut self) -> Result<()> {
        while !self.state.cancelled {
            self.queue
                .blocking_dispatch(&mut self.state)
                .map_err(|err| anyhow!("falando com o compositor: {err}"))?;
        }
        tracing::debug!("seleção assumida por outro cliente");
        Ok(())
    }
}

/// Publica o payload numa thread própria e volta: a seleção precisa sobreviver a
/// quem pediu (a UI fecha o popup logo depois, a CLI sai).
pub fn spawn(payload: Payload) -> Result<()> {
    let server = Server::start(payload)?;
    std::thread::Builder::new()
        .name("transferencia-selection".into())
        .spawn(move || {
            if let Err(err) = server.run() {
                tracing::warn!("servindo a seleção: {err:#}");
            }
        })
        .context("criando a thread da seleção")?;
    Ok(())
}

/// Confere se o compositor aceita uma fonte de dados (usado pelo `doctor`).
pub fn check_support() -> Result<()> {
    let conn = Connection::connect_to_env().context("conectando ao Wayland")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)
        .map_err(|err| anyhow!("lendo os globais do Wayland: {err}"))?;
    let qh = queue.handle();
    let manager: ZwlrDataControlManagerV1 = globals
        .bind(&qh, 1..=1, ())
        .map_err(|err| anyhow!("compositor não oferece zwlr_data_control_manager_v1 ({err})"))?;
    let source = manager.create_data_source(&qh, ());
    source.destroy();
    let mut state = State {
        parts: Arc::new(Vec::new()),
        cancelled: false,
    };
    queue
        .roundtrip(&mut state)
        .map_err(|err| anyhow!("falando com o compositor: {err}"))?;
    Ok(())
}

/// Escreve o conteúdo pedido. Fora do laço de eventos: um conteúdo grande (uma
/// imagem de MBs) não pode segurar o atendimento dos outros pedidos.
fn send(fd: OwnedFd, bytes: Arc<Vec<u8>>) {
    std::thread::spawn(move || {
        let mut file = std::fs::File::from(fd);
        if let Err(err) = file.write_all(&bytes) {
            tracing::debug!("alvo abandonou a leitura: {err}");
        }
    });
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

impl Dispatch<ZwlrDataControlDeviceV1, ()> for State {
    /// A seleção de outros clientes não interessa aqui: quem observa é o watcher.
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlDeviceV1,
        _event: <ZwlrDataControlDeviceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }

    // DENTRO do impl: a macro gera o `event_created_child` que dá user-data à
    // oferta criada pelo evento `data_offer` que o compositor manda para nós.
    event_created_child!(State, ZwlrDataControlDeviceV1, [
        zwlr_data_control_device_v1::EVT_DATA_OFFER_OPCODE => (ZwlrDataControlOfferV1, ()),
    ]);
}

impl Dispatch<ZwlrDataControlOfferV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwlrDataControlOfferV1,
        _event: <ZwlrDataControlOfferV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwlrDataControlSourceV1, ()> for State {
    fn event(
        state: &mut Self,
        _source: &ZwlrDataControlSourceV1,
        event: <ZwlrDataControlSourceV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_data_control_source_v1::Event::Send { mime_type, fd } => {
                match state.parts.iter().find(|(mime, _)| *mime == mime_type) {
                    Some((_, bytes)) => send(fd, Arc::clone(bytes)),
                    None => tracing::debug!("alvo pediu {mime_type}, que não oferecemos"),
                }
            }
            zwlr_data_control_source_v1::Event::Cancelled => state.cancelled = true,
            _ => {}
        }
    }
}
