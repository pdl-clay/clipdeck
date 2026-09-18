//! Daemon e popup num único processo.
//!
//! O daemon é a instância única (`run_single_instance`): ela observa o clipboard
//! numa thread própria e abre a janela quando recebe `--toggle` por ativação
//! D-Bus. A janela é uma layer surface centralizada (mesmo mecanismo do
//! cosmic-launcher), destruída ao fechar.

use std::fmt::{self, Display};
use std::str::FromStr;
use std::time::Duration;

use cosmic::app::{Core, CosmicFlags, Task};
use cosmic::cctk::sctk::shell::wlr_layer::{Anchor, KeyboardInteractivity, Layer};
use cosmic::iced::event::listen_raw;
use cosmic::iced::keyboard::key::Named;
use cosmic::iced::keyboard::Key;
use cosmic::iced::runtime::core::event::wayland::LayerEvent;
use cosmic::iced::runtime::core::event::{PlatformSpecific, wayland};
use cosmic::iced::runtime::core::layout::Limits;
use cosmic::iced::runtime::core::window::{Event as WindowEvent, Id as SurfaceId};
use cosmic::iced::runtime::platform_specific::wayland::layer_surface::{
    IcedMargin, IcedOutput, SctkLayerSurfaceSettings,
};
use cosmic::iced::widget::{operation, scrollable::RelativeOffset};
use cosmic::iced::Subscription;
use cosmic::surface::action::{LiveSettings, app_layer_shell, destroy_layer_shell};
use cosmic::surface::surface_task;
use cosmic::widget::container;
use cosmic::widget::space::vertical as vertical_space;
use cosmic::Element;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::clipboard;
use crate::config::{APP_ID, Config};
use crate::db::{Db, Item};
use crate::paste;
use crate::view;
use crate::watcher;

/// Itens carregados por abertura do popup.
const LIST_LIMIT: usize = 200;
/// Intervalo para o segundo clique contar como duplo clique.
const DOUBLE_CLICK: std::time::Duration = std::time::Duration::from_millis(450);

/// Id da lista rolável, para manter o item focado visível.
pub static SCROLL_ID: std::sync::LazyLock<cosmic::widget::Id> =
    std::sync::LazyLock::new(|| cosmic::widget::Id::new("transferencia-list"));

/// O que fazer ao subir/ativar a instância.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    /// Sobe oculto (autostart) e só observa.
    Daemon,
    /// Alterna o popup (usado pelo atalho Super+V).
    Toggle,
    Show,
    Hide,
}

impl Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Action::Daemon => "Daemon",
            Action::Toggle => "Toggle",
            Action::Show => "Show",
            Action::Hide => "Hide",
        })
    }
}

impl FromStr for Action {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        match text.trim().trim_matches('"') {
            "Daemon" => Ok(Action::Daemon),
            "Toggle" => Ok(Action::Toggle),
            "Show" => Ok(Action::Show),
            "Hide" => Ok(Action::Hide),
            _ => Err(()),
        }
    }
}

/// Flags da instância: a ação pedida mais o que o processo já abriu.
pub struct Flags {
    pub action: Action,
    pub cfg: Config,
    pub db: Db,
}

impl CosmicFlags for Flags {
    type SubCommand = Action;
    type Args = Vec<String>;

    fn action(&self) -> Option<&Action> {
        Some(&self.action)
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    InputChanged(String),
    /// Um clique: seleciona e mostra a prévia (dois cliques colam).
    SelectIndex(usize),
    ActivateFocused { plain: bool },
    RemoveFocused,
    TogglePinFocused,
    FocusNext,
    FocusPrevious,
    Resized(cosmic::iced::Size),
    Hide,
    Layer(LayerEvent, SurfaceId),
}

pub struct App {
    core: Core,
    cfg: Config,
    db: Db,
    items: Vec<Item>,
    thumb_handles: Vec<Option<cosmic::widget::image::Handle>>,
    /// Prévia do item selecionado: (id do item, imagem) — o par evita recarregar
    /// a cada quadro e detecta quando a seleção mudou.
    preview_handle: Option<(i64, Option<cosmic::widget::image::Handle>)>,
    last_click: Option<(usize, std::time::Instant)>,
    query: String,
    focused: usize,
    window_id: SurfaceId,
    surface_size: Option<cosmic::iced::Size>,
    visible: bool,
    status: Option<String>,
}

/// Entra no laço do libcosmic como instância única.
pub fn run(action: Action) -> Result<(), Box<dyn std::error::Error>> {
    let cfg = Config::load();
    let db = Db::open(&crate::config::db_path())?;

    let settings = cosmic::app::Settings::default()
        .antialiasing(true)
        .client_decorations(false)
        .debug(false)
        .default_text_size(16.0)
        .no_main_window(true)
        .exit_on_close(false);

    cosmic::app::run_single_instance::<App>(settings, Flags { action, cfg, db })?;
    Ok(())
}

impl App {
    fn refresh(&mut self) {
        let started = std::time::Instant::now();
        let result = if self.query.trim().is_empty() {
            self.db.recent(LIST_LIMIT)
        } else {
            self.db.search(&self.query, LIST_LIMIT)
        };
        let elapsed = started.elapsed();
        if elapsed > Duration::from_millis(100) {
            tracing::warn!("consulta ao histórico levou {elapsed:?}");
        }
        match result {
            Ok(items) => {
                self.items = items;
                // Handles criados uma vez por carga: o iced reaproveita a textura
                // em vez de reenviar a imagem a cada quadro.
                self.thumb_handles = self
                    .items
                    .iter()
                    .map(|item| {
                        item.thumb.as_ref().map(|bytes| {
                            cosmic::widget::image::Handle::from_bytes(bytes.clone())
                        })
                    })
                    .collect();
                if self.focused >= self.items.len() {
                    self.focused = 0;
                }
                self.load_preview();
            }
            Err(err) => {
                warn!("consultando o histórico: {err:#}");
                self.status = Some(format!("erro ao consultar o histórico: {err}"));
            }
        }
    }

    fn show(&mut self) -> Task<Message> {
        if self.visible {
            return Task::none();
        }
        // Limpa a busca antes de recarregar: a lista aberta é sempre a recente.
        self.query.clear();
        self.focused = 0;
        // Relê a config a cada abertura: popup e colagem seguem o arquivo sem
        // precisar reiniciar (os limites de captura valem no watcher, que sobe uma vez).
        self.cfg = Config::load();
        self.refresh();
        self.visible = true;
        info!("popup aberto com {} item(ns)", self.items.len());

        surface_task(app_layer_shell::<Self>(
            |_app: &Self| LiveSettings {
                padding: Some(IcedMargin::default()),
                // Tela cheia: o recorte e o blur ficam só no painel (BlurSurface).
                corners: None,
                // Não deixa o tema ligar blur na superfície inteira.
                blur: Some(false),
            },
            |app: &mut Self| SctkLayerSurfaceSettings {
                id: app.window_id,
                layer: Layer::Overlay,
                keyboard_interactivity: KeyboardInteractivity::Exclusive,
                // Cobre a saída inteira: o ponteiro nunca sai da nossa superfície,
                // então o compositor não devolve o foco para outra janela enquanto
                // o usuário usa o mouse — e o clique fora fecha (somos nós que
                // recebemos esse clique).
                anchor: Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT,
                input_zone: None,
                output: IcedOutput::Active,
                namespace: "transferencia".to_string(),
                size: None,
                exclusive_zone: -1,
                margin: IcedMargin::default(),
                size_limits: Limits::NONE.min_width(1.0).min_height(1.0),
            },
            None,
        ))
    }

    /// Aplica blur só no retângulo do painel; o resto da superfície fica
    /// transparente (e sem blur), então a tela não é afetada.
    fn blur_panel(&self) -> Task<Message> {
        let Some(size) = self.surface_size else {
            return Task::none();
        };
        if !self.core.system_theme().cosmic().frosted_system_interface {
            return Task::none();
        }
        let rect = cosmic::iced::Rectangle {
            x: (size.width - self.cfg.popup_width) / 2.0,
            y: (size.height - self.cfg.popup_height) / 2.0,
            width: self.cfg.popup_width,
            height: self.cfg.popup_height,
        };
        cosmic::iced::runtime::task::effect(cosmic::iced::runtime::Action::PlatformSpecific(
            cosmic::iced::runtime::platform_specific::Action::Wayland(
                cosmic::iced::runtime::platform_specific::wayland::Action::BlurSurface(
                    self.window_id,
                    Some(vec![rect]),
                ),
            ),
        ))
    }

    fn hide(&mut self) -> Task<Message> {
        if !self.visible {
            return Task::none();
        }
        info!("popup fechado");
        self.visible = false;
        self.query.clear();
        self.focused = 0;
        self.status = None;
        surface_task(destroy_layer_shell::<Message>(self.window_id))
    }

    fn toggle(&mut self) -> Task<Message> {
        if self.visible { self.hide() } else { self.show() }
    }

    /// Um clique: seleciona (a prévia acompanha) sem colar.
    fn select(&mut self, index: usize) -> Task<Message> {
        if index >= self.items.len() {
            return Task::none();
        }
        self.focused = index;
        self.load_preview();
        self.scroll_to_focused()
    }

    /// Carrega a prévia do item selecionado, se ainda não estiver em cache.
    fn load_preview(&mut self) {
        let Some(item) = self.items.get(self.focused) else {
            self.preview_handle = None;
            return;
        };
        let id = item.id;
        if matches!(&self.preview_handle, Some((cached, _)) if *cached == id) {
            return;
        }
        let handle = match self.db.preview_of(id) {
            Ok(Some(bytes)) => Some(cosmic::widget::image::Handle::from_bytes(bytes)),
            Ok(None) => None,
            Err(err) => {
                warn!("lendo a prévia do item {id}: {err:#}");
                None
            }
        };
        self.preview_handle = Some((id, handle));
    }

    /// Clique numa linha: primeiro seleciona, o segundo (dentro do intervalo)
    /// cola.
    fn clicked(&mut self, index: usize) -> Task<Message> {
        let now = std::time::Instant::now();
        let double = self
            .last_click
            .is_some_and(|(previous, at)| previous == index && now.duration_since(at) < DOUBLE_CLICK);
        self.last_click = Some((index, now));
        if double {
            tracing::debug!("duplo clique: colando o item {index}");
            self.activate(index, false)
        } else {
            tracing::debug!("clique: selecionando o item {index}");
            self.select(index)
        }
    }

    fn activate(&mut self, index: usize, plain: bool) -> Task<Message> {
        let started = std::time::Instant::now();
        let Some(item) = self.items.get(index).cloned() else {
            return self.hide();
        };
        // A listagem não carrega o blob; para imagens é preciso buscar de novo.
        let item = match self.db.get(item.id) {
            Ok(Some(full)) => full,
            _ => item,
        };

        let payload = match clipboard::payload_for_item(&item, plain) {
            Ok(payload) => payload,
            Err(err) => {
                self.status = Some(format!("não deu para copiar: {err}"));
                return Task::none();
            }
        };
        if let Err(err) = clipboard::set(payload) {
            self.status = Some(format!("não deu para copiar: {err}"));
            return Task::none();
        }
        tracing::debug!("item {} no clipboard em {:?}", item.id, started.elapsed());

        let task = self.hide();
        if self.cfg.auto_paste {
            let delay = self.cfg.paste_delay_ms;
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(delay));
                if let Err(err) = paste::send_combo(paste::Combo::paste()) {
                    warn!("auto-paste falhou: {err:#}");
                }
            });
        }
        task
    }

    fn remove_focused(&mut self) -> Task<Message> {
        let Some(item) = self.items.get(self.focused) else {
            return Task::none();
        };
        let id = item.id;
        if let Err(err) = self.db.remove(id) {
            self.status = Some(format!("não deu para remover: {err}"));
            return Task::none();
        }
        self.refresh();
        Task::none()
    }

    fn toggle_pin_focused(&mut self) -> Task<Message> {
        let Some(item) = self.items.get(self.focused) else {
            return Task::none();
        };
        let id = item.id;
        match self.db.toggle_pinned(id) {
            Ok(Some(pinned)) => {
                info!("item {id} {}", if pinned { "fixado" } else { "desfixado" });
                self.refresh();
                self.focused = self
                    .items
                    .iter()
                    .position(|item| item.id == id)
                    .unwrap_or(self.focused);
            }
            Ok(None) => {}
            Err(err) => self.status = Some(format!("não deu para fixar: {err}")),
        }
        Task::none()
    }

    fn focus(&mut self, delta: isize) -> Task<Message> {
        if self.items.is_empty() {
            return Task::none();
        }
        let len = self.items.len() as isize;
        self.focused = ((self.focused as isize + delta).rem_euclid(len)) as usize;
        self.load_preview();
        self.scroll_to_focused()
    }

    fn scroll_to_focused(&self) -> Task<Message> {
        let total = self.items.len().saturating_sub(1).max(1) as f32;
        operation::snap_to(
            SCROLL_ID.clone(),
            RelativeOffset {
                x: None,
                y: Some(self.focused as f32 / total),
            },
        )
    }
}

impl cosmic::Application for App {
    type Executor = cosmic::executor::single::Executor;
    type Flags = Flags;
    type Message = Message;

    const APP_ID: &'static str = APP_ID;

    fn init(mut core: Core, flags: Flags) -> (Self, Task<Message>) {
        let Flags { action, cfg, db } = flags;
        let mut status = None;
        info!("transferencia iniciado em modo {action}");
        // Como o cosmic-launcher: superfície de sistema, para o tema aplicar a
        // regra de vidro (`frosted_system_interface`) às nossas superfícies.
        core.set_app_type(cosmic::core::AppType::System);

        // Conexão própria para o watcher: ele escreve, a UI lê.
        match Db::open(&crate::config::db_path()) {
            Ok(watch_db) => {
                if let Err(err) = watcher::spawn(cfg.clone(), watch_db) {
                    warn!("captura indisponível: {err:#}");
                    status = Some(format!("captura indisponível: {err}"));
                }
                // Imagens antigas ganham prévia sem travar a abertura do popup.
                if let Ok(backfill_db) = Db::open(&crate::config::db_path()) {
                    watcher::backfill_previews(backfill_db);
                }
            }
            Err(err) => {
                warn!("captura indisponível: {err:#}");
                status = Some(format!("captura indisponível: {err}"));
            }
        }

        let mut app = App {
            core,
            cfg,
            db,
            items: Vec::new(),
            thumb_handles: Vec::new(),
            preview_handle: None,
            last_click: None,
            query: String::new(),
            focused: 0,
            window_id: SurfaceId::unique(),
            surface_size: None,
            visible: false,
            status,
        };
        app.refresh();

        let task = match action {
            Action::Daemon | Action::Hide => Task::none(),
            Action::Show | Action::Toggle => app.show(),
        };
        (app, task)
    }

    fn core(&self) -> &Core {
        &self.core
    }

    fn core_mut(&mut self) -> &mut Core {
        &mut self.core
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        tracing::debug!("mensagem recebida: {message:?}");
        let started = std::time::Instant::now();
        let task = match message {
            Message::InputChanged(value) => {
                self.query = value;
                self.focused = 0;
                self.refresh();
                self.scroll_to_focused()
            }
            Message::SelectIndex(index) => self.clicked(index),
            Message::ActivateFocused { plain } => self.activate(self.focused, plain),
            Message::RemoveFocused => self.remove_focused(),
            Message::TogglePinFocused => self.toggle_pin_focused(),
            Message::FocusNext => self.focus(1),
            Message::FocusPrevious => self.focus(-1),
            Message::Resized(size) => {
                self.surface_size = Some(size);
                self.blur_panel()
            }
            Message::Hide => self.hide(),
            Message::Layer(event, id) => {
                if id != self.window_id {
                    return Task::none();
                }
                match event {
                    LayerEvent::Unfocused => self.hide(),
                    LayerEvent::Focused | LayerEvent::Done => Task::none(),
                }
            }
        };
        let elapsed = started.elapsed();
        if elapsed > Duration::from_millis(100) {
            tracing::warn!("tratando mensagem levou {elapsed:?}");
        }
        task
    }

    /// O `--toggle` chega aqui como ativação D-Bus da instância que já roda.
    fn dbus_activation(&mut self, msg: cosmic::dbus_activation::Message) -> Task<Message> {
        use cosmic::dbus_activation::Details;
        match msg.msg {
            Details::Activate => {
                info!("ativação recebida (activate)");
                self.toggle()
            }
            Details::ActivateAction { action, .. } => {
                info!("ativação recebida (action={action})");
                match Action::from_str(&action) {
                    Ok(Action::Toggle) => self.toggle(),
                    Ok(Action::Show) => self.show(),
                    Ok(Action::Hide) => self.hide(),
                    Ok(Action::Daemon) | Err(()) => Task::none(),
                }
            }
            Details::Open { .. } => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Self::Message> {
        // Sem janela principal (`no_main_window`): nada é desenhado aqui.
        container(vertical_space()).into()
    }

    fn view_window(&self, id: SurfaceId) -> Element<'_, Self::Message> {
        if id == self.window_id {
            return view::overlay(view::popup(
                &self.items,
                &self.thumb_handles,
                self.preview_handle.as_ref().and_then(|(_, handle)| handle.as_ref()),
                &self.query,
                self.focused,
                self.status.as_deref(),
                self.cfg.popup_width,
                self.cfg.popup_height,
            ));
        }
        container(vertical_space()).into()
    }

    fn subscription(&self) -> Subscription<Message> {
        listen_raw(|event, _status, _id| match event {
            cosmic::iced::Event::Mouse(cosmic::iced::mouse::Event::ButtonPressed(button)) => {
                tracing::debug!("clique recebido: {button:?}");
                None
            }
            cosmic::iced::Event::Window(WindowEvent::Opened { size, .. }) => {
                Some(Message::Resized(size))
            }
            cosmic::iced::Event::Window(WindowEvent::Resized(size)) => {
                Some(Message::Resized(size))
            }
            cosmic::iced::Event::PlatformSpecific(PlatformSpecific::Wayland(
                wayland::Event::Layer(event, _, id),
            )) => Some(Message::Layer(event, id)),
            cosmic::iced::Event::Keyboard(cosmic::iced::keyboard::Event::KeyPressed {
                key,
                modifiers,
                ..
            }) => match key {
                Key::Named(Named::ArrowUp) => Some(Message::FocusPrevious),
                Key::Named(Named::ArrowDown) => Some(Message::FocusNext),
                Key::Named(Named::Enter) => Some(Message::ActivateFocused {
                    plain: modifiers.shift(),
                }),
                Key::Named(Named::Escape) => Some(Message::Hide),
                Key::Named(Named::Delete) => Some(Message::RemoveFocused),
                Key::Named(Named::Tab) => Some(Message::FocusNext),
                Key::Character(text) if modifiers.control() && text.as_str() == "p" => {
                    Some(Message::TogglePinFocused)
                }
                _ => None,
            },
            _ => None,
        })
    }
}
