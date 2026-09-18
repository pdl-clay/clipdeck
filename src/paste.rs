//! Injeção de teclas via `zwp_virtual_keyboard_manager_v1`.
//!
//! Serve ao auto-paste (Ctrl+V depois de escolher um item) e ao subcomando
//! `transferencia key`, que envia uma combinação para a janela em foco sem
//! roubar o foco.
//!
//! No fio os keycodes são os do kernel (evdev): a spec manda somar 8 para obter
//! o keycode xkb. O keymap é enviado como texto xkb, terminado em NUL.

use std::fmt::{self, Display};
use std::io::Write;
use std::os::fd::AsFd;
use std::str::FromStr;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use wayland_client::{
    Connection, Dispatch, EventQueue, Proxy, QueueHandle,
    globals::{GlobalListContents, registry_queue_init},
    protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1,
    zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};
use xkbcommon::xkb;

/// Keycodes evdev.
const KEY_ESCAPE: u32 = 1;
const KEY_TAB: u32 = 15;
const KEY_RETURN: u32 = 28;
const KEY_LEFTCTRL: u32 = 29;
const KEY_V: u32 = 47;
const KEY_UP: u32 = 103;
const KEY_DOWN: u32 = 108;
const KEY_DELETE: u32 = 111;
const KEY_LEFTMETA: u32 = 125;

const KEY_RELEASED: u32 = 0;
const KEY_PRESSED: u32 = 1;

/// Espera após enviar o keymap, antes da primeira tecla.
const KEYMAP_SETTLE: Duration = Duration::from_millis(20);
/// Espera entre pressionar modificador, tecla e soltar.
const KEY_STEP: Duration = Duration::from_millis(8);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Modifier {
    Ctrl,
    Super,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    V,
    Up,
    Down,
    Return,
    Escape,
    Delete,
    Tab,
}

impl Key {
    fn keycode(self) -> u32 {
        match self {
            Key::V => KEY_V,
            Key::Up => KEY_UP,
            Key::Down => KEY_DOWN,
            Key::Return => KEY_RETURN,
            Key::Escape => KEY_ESCAPE,
            Key::Delete => KEY_DELETE,
            Key::Tab => KEY_TAB,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Combo {
    pub modifier: Option<Modifier>,
    pub key: Key,
}

impl Combo {
    /// Ctrl+V: usado pelo auto-paste.
    pub const fn paste() -> Self {
        Combo {
            modifier: Some(Modifier::Ctrl),
            key: Key::V,
        }
    }
}

impl Display for Combo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.modifier {
            Some(Modifier::Ctrl) => write!(f, "ctrl+")?,
            Some(Modifier::Super) => write!(f, "super+")?,
            None => {}
        }
        f.write_str(match self.key {
            Key::V => "v",
            Key::Up => "up",
            Key::Down => "down",
            Key::Return => "enter",
            Key::Escape => "escape",
            Key::Delete => "delete",
            Key::Tab => "tab",
        })
    }
}

impl FromStr for Combo {
    type Err = ();

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let normalized = text.trim().to_ascii_lowercase().replace(' ', "");
        let (modifier, key) = match normalized.split_once('+') {
            Some((modifier, key)) => {
                let modifier = match modifier {
                    "ctrl" | "control" => Modifier::Ctrl,
                    "super" | "meta" | "logo" => Modifier::Super,
                    _ => return Err(()),
                };
                (Some(modifier), key)
            }
            None => (None, normalized.as_str()),
        };
        let key = match key {
            "v" => Key::V,
            "up" => Key::Up,
            "down" => Key::Down,
            "enter" | "return" => Key::Return,
            "escape" | "esc" => Key::Escape,
            "delete" | "del" => Key::Delete,
            "tab" => Key::Tab,
            _ => return Err(()),
        };
        Ok(Combo { modifier, key })
    }
}

struct State;

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

impl Dispatch<ZwpVirtualKeyboardManagerV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardManagerV1,
        _event: <ZwpVirtualKeyboardManagerV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<ZwpVirtualKeyboardV1, ()> for State {
    fn event(
        _state: &mut Self,
        _proxy: &ZwpVirtualKeyboardV1,
        _event: <ZwpVirtualKeyboardV1 as Proxy>::Event,
        _data: &(),
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
    ) {
    }
}

fn setup() -> Result<(Connection, EventQueue<State>, State, ZwpVirtualKeyboardV1)> {
    let conn = Connection::connect_to_env().context("conectando ao Wayland")?;
    let (globals, queue) = registry_queue_init::<State>(&conn)
        .map_err(|err| anyhow!("lendo os globais do Wayland: {err}"))?;
    let qh = queue.handle();

    let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|err| {
        anyhow!("compositor não oferece zwp_virtual_keyboard_manager_v1 ({err})")
    })?;
    let seat: WlSeat = globals
        .bind(&qh, 1..=9, ())
        .map_err(|err| anyhow!("nenhum seat Wayland: {err}"))?;

    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    Ok((conn, queue, State, keyboard))
}

/// Verifica se o compositor permite criar teclados virtuais.
pub fn check_support() -> Result<()> {
    let (_conn, _queue, _state, keyboard) = setup()?;
    keyboard.destroy();
    Ok(())
}

/// Envia a combinação para a superfície com foco, sem mudar o foco.
pub fn send_combo(combo: Combo) -> Result<()> {
    let (conn, mut queue, mut state, keyboard) = setup()?;
    let started = Instant::now();
    let stamp = || started.elapsed().as_millis() as u32;

    let keymap = us_keymap()?;
    let keymap_text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);

    let modifier = match combo.modifier {
        Some(Modifier::Ctrl) => Some((KEY_LEFTCTRL, mod_mask(&keymap, xkb::MOD_NAME_CTRL)?)),
        Some(Modifier::Super) => Some((KEY_LEFTMETA, mod_mask(&keymap, xkb::MOD_NAME_LOGO)?)),
        None => None,
    };

    // O compositor só aceita `key` depois de receber o keymap.
    let mut keymap_file = tempfile::tempfile().context("criando arquivo do keymap")?;
    keymap_file
        .write_all(keymap_text.as_bytes())
        .context("escrevendo o keymap")?;
    keymap_file.write_all(b"\0")?;
    keyboard.keymap(
        xkb::KEYMAP_FORMAT_TEXT_V1,
        keymap_file.as_fd(),
        (keymap_text.len() + 1) as u32,
    );
    flush(&conn, &mut queue, &mut state)?;
    std::thread::sleep(KEYMAP_SETTLE);

    if let Some((keycode, mask)) = modifier {
        keyboard.key(stamp(), keycode, KEY_PRESSED);
        keyboard.modifiers(mask, 0, 0, 0);
        flush(&conn, &mut queue, &mut state)?;
        std::thread::sleep(KEY_STEP);
    }

    keyboard.key(stamp(), combo.key.keycode(), KEY_PRESSED);
    flush(&conn, &mut queue, &mut state)?;
    std::thread::sleep(KEY_STEP);

    keyboard.key(stamp(), combo.key.keycode(), KEY_RELEASED);
    if let Some((keycode, _)) = modifier {
        keyboard.key(stamp(), keycode, KEY_RELEASED);
        keyboard.modifiers(0, 0, 0, 0);
    }
    flush(&conn, &mut queue, &mut state)?;

    keyboard.destroy();
    conn.flush().context("descarregando pedidos para o compositor")?;
    Ok(())
}

fn mod_mask(keymap: &xkb::Keymap, name: &str) -> Result<u32> {
    let index = keymap.mod_get_index(name);
    if index == xkb::MOD_INVALID {
        bail!("keymap us não define o modificador {name}");
    }
    Ok(1u32 << index)
}

fn flush(conn: &Connection, queue: &mut EventQueue<State>, state: &mut State) -> Result<()> {
    queue
        .roundtrip(state)
        .map_err(|err| anyhow!("falando com o compositor: {err}"))?;
    conn.flush().context("descarregando pedidos")?;
    Ok(())
}

/// Keymap `us` compilado: garante que os keycodes enviados virem as teclas
/// esperadas, independentemente do layout do teclado físico.
fn us_keymap() -> Result<xkb::Keymap> {
    let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
    xkb::Keymap::new_from_names(
        &context,
        "",
        "",
        "us",
        "",
        None,
        xkb::KEYMAP_COMPILE_NO_FLAGS,
    )
    .context("compilando keymap us com libxkbcommon")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(text: &str) -> Combo {
        Combo::from_str(text).expect(text)
    }

    #[test]
    fn parse_aceita_variacoes_de_combo() {
        assert_eq!(
            combo("ctrl+v"),
            Combo {
                modifier: Some(Modifier::Ctrl),
                key: Key::V
            }
        );
        assert_eq!(
            combo(" Ctrl + V "),
            Combo {
                modifier: Some(Modifier::Ctrl),
                key: Key::V
            }
        );
        assert_eq!(
            combo("Meta+V"),
            Combo {
                modifier: Some(Modifier::Super),
                key: Key::V
            }
        );
        assert_eq!(
            combo("esc"),
            Combo {
                modifier: None,
                key: Key::Escape
            }
        );
        assert_eq!(
            combo("Down"),
            Combo {
                modifier: None,
                key: Key::Down
            }
        );
        assert_eq!(
            combo("enter"),
            Combo {
                modifier: None,
                key: Key::Return
            }
        );
        assert!(Combo::from_str("alt+v").is_err());
        assert!(Combo::from_str("ctrl+z").is_err());
        assert!(Combo::from_str("").is_err());
    }

    #[test]
    fn display_mostra_a_combinacao() {
        assert_eq!(combo("ctrl+v").to_string(), "ctrl+v");
        assert_eq!(combo("super+v").to_string(), "super+v");
        assert_eq!(combo("escape").to_string(), "escape");
        assert_eq!(combo("enter").to_string(), "enter");
    }

    #[test]
    fn keymap_us_compila_com_os_modificadores_usados() {
        let keymap = us_keymap().expect("keymap us");
        assert_ne!(keymap.mod_get_index(xkb::MOD_NAME_CTRL), xkb::MOD_INVALID);
        assert_ne!(keymap.mod_get_index(xkb::MOD_NAME_LOGO), xkb::MOD_INVALID);
        let text = keymap.get_as_string(xkb::KEYMAP_FORMAT_TEXT_V1);
        assert!(text.contains("xkb_keymap"), "keymap textual inesperado");
        assert!(mod_mask(&keymap, xkb::MOD_NAME_CTRL).is_ok());
        assert!(mod_mask(&keymap, "ModInexistente").is_err());
    }
}
