# Plano de implementação — transferencia

Baseado em `docs/superpowers/specs/2026-09-18-transferencia-design.md`.
Regra: cada fase termina com verificação observável antes da seguinte.

## Estrutura de arquivos

```
Cargo.toml
src/main.rs        clap, logging, dispatch (--daemon/--toggle/subcomandos)
src/config.rs      Config (TOML) + defaults + load/save
src/db.rs          schema, FTS5, upsert/dedupe, busca, pins, evict, stats
src/clipboard.rs   classify()/is_secret_hint() puros + set() via wl-copy
src/watcher.rs     thread Wayland data-control -> mpsc<Captured>
src/app.rs         libcosmic Application: estado, single-instance, toggle, surface
src/view.rs        UI do popup (busca, lista, rodapé de teclas)
src/paste.rs       injeção de teclas via zwp_virtual_keyboard (ctrl+v, super+v)
src/shortcut.rs    patch/undo em ~/.config/cosmic/.../Shortcuts/v1/custom
src/autostart.rs   .desktop em ~/.config/autostart
src/cli.rs         handlers dos subcomandos
```

## Fase 1 — Fundação (eu)
Arquivos: `Cargo.toml`, `main.rs`, `config.rs`, `db.rs`, `clipboard.rs`, `cli.rs`.
- `db.rs` completo: schema v1, triggers FTS, `upsert` (Inserted|Bumped), `search`,
  `recent`, `set_pinned`, `remove`, `clear`, `get`, `stats`, `evict`.
- `clipboard.rs`: `classify` (ordem da spec §7), `is_secret_hint`, `set` (wl-copy).
- `cli.rs`: `list`, `search`, `copy`, `pin`, `remove`, `clear`, `status`, `doctor`.
- Testes: db (upsert/dedupe/pin/evict/FTS) e clipboard (ordem de MIME, hint).
- **Verificação**: `cargo test` verde; `transferencia list` lê um DB semeado.

## Fase 2 — Captura (delegada em paralelo com a Fase 3)
Arquivo: `watcher.rs` (interface fixa: `spawn(tx) -> JoinHandle`, emite `Captured`
com `kind/mime/text/html/bytes/sha256`).
- `cosmic::cctk` como fonte dos tipos wayland; preferir `ext_data_control`, cair
  para `wlr_data_control`; `std::io::pipe()` + `offer.receive`.
- Integração no daemon: thread + canal → `db.upsert` + evict.
- **Verificação**: `wl-copy` de texto/imagem/arquivo aparece no `transferencia list`.

## Fase 3 — UI + atalho (eu)
Arquivos: `app.rs`, `view.rs`.
- `run_single_instance` + `no_main_window(true)` + `exit_on_close(false)`; `--toggle`
  mostra, `Esc`/seleção destrói a surface (`app_layer_shell` + `destroy_layer_surface`).
- Janela: layer overlay, sem âncora (centralizada), `KeyboardInteractivity::Exclusive`,
  `exclusive_zone = -1`, namespace `transferencia`, tema COSMIC.
- Busca com foco automático; ↑/↓, Enter, Shift+Enter, Delete, Ctrl+P, Esc.
- **Verificação**: screenshot do popup, `wl-paste` após selecionar, `key super+v`
  abrindo o popup.

## Fase 4 — Auto-paste (delegada em paralelo com a Fase 3)
Arquivo: `paste.rs` (interface: `send_combo(Combo) -> Result<()>`; `Key` no CLI).
- `zwp_virtual_keyboard_manager_v1` + `wayland-protocols-misc` + `xkbcommon`.
- **Verificação**: `transferencia key ctrl+v` digita num campo focado.

## Fase 5 — Instalação e polimento (eu)
`shortcut.rs`, `autostart.rs`, `install`/`uninstall`, `doctor` completo, README.
- **Verificação**: Super+V abre o popup; Super+Return continua abrindo o alacritty
  (prova de que o arquivo de atalhos segue válido).

## Riscos ativos
- Versão de `wayland-client` do `cctk` vs `wayland-protocols-misc` (helper tem
  conexão própria → duas cópias não conflitam).
- Iced com zero janelas: se o loop encerrar, criar surface "dummy" 1×1 (padrão do
  cosmic-launcher).
- `wl-copy` forka: usar `Command::spawn` + `wait` para não travar o executor.
