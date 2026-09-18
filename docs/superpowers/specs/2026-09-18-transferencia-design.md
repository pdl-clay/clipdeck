# transferencia — gerenciador de área de transferência nativo para COSMIC

**Status:** aprovado (design) — pronto para virar plano de implementação
**Data:** 2026-09-18
**Alvo:** COSMIC 1.8.0 / Wayland, CachyOS, Rust 1.96.1
**Licença:** MIT

---

## 1. Objetivo

Oferecer no COSMIC o que ele hoje não tem: um histórico da área de transferência
aberto por **Super+V**, com lista dos itens copiados, busca, navegação por teclado e
cola direta no aplicativo em foco.

Entrega: um único binário `transferencia`, autostartado, que observa o clipboard,
persiste o histórico em SQLite, abre um popup centralizado estilo `cosmic-launcher`
no Super+V e, ao escolher um item, o devolve ao clipboard (e opcionalmente cola).

## 2. Fatos verificados do ambiente (evidência, não suposição)

| Verificação | Resultado | Como foi verificado |
|---|---|---|
| `zwlr_data_control_manager_v1` no cosmic-comp | presente | `wl-paste --list-types` e `wl-paste --watch` executados na sessão viva (exit 124 = vigiando sem erro) |
| `ext_data_control_manager_v1` | presente | `strings /usr/bin/cosmic-comp` |
| `zwlr_layer_shell_v1` | presente | idem |
| `zwp_virtual_keyboard_manager_v1` | presente | idem |
| `xdg_activation_v1` | presente | idem |
| Atalhos globais do COSMIC | `Spawn("...")` via cosmic-config | `~/.config/cosmic/com.system76.CosmicSettings.Shortcuts/v1/custom` (ex.: Super+Return → alacritty) |
| `wl-copy` / `wl-paste` | instalados (wl-clipboard 1:2.3.0) | `command -v` |
| `libcosmic` com `surface::action::LayerShell`, `run_single_instance`, activation token | presente (rev `87ab817`, v1.0.0) | fonte em `~/.cargo/git/checkouts/libcosmic-41009aea1d72760b/87ab817/src/` |
| Padrão de referência "daemon + atalho + surface" | `pop-os/cosmic-launcher` | `src/app.rs` (usa `no_main_window(true)`, `exit_on_close(false)`, `app_layer_shell`, `KeyboardInteractivity::Exclusive`) |
| Toolchain | Rust 1.96.1, crates.io e github acessíveis | `cargo --version`, HTTP 200 |
| `cliphist` / `wl-clip-persist` | instalados, **não** em execução | `pgrep` — não conflitam; não serão usados |

## 3. Não-objetivos (YAGNI)

Applet no painel; criptografia em repouso; emoji/kaomoji/snippets; sincronização;
suporte a X11; seleção primária; OCR; múltiplas instâncias/DBs.

## 4. Arquitetura

**Processo único, dois papéis, duas threads.**

```
main.rs (clap)
 ├── --daemon     → app libcosmic + watcher (autostart)
 ├── --toggle     → ativa a instância única (Super+V); se não houver, sobe e mostra
 ├── list|search|clear|remove|copy|pin|status|doctor|install|uninstall|key  (CLI)
 │
 processo daemon
 ├── app.rs    — libcosmic Application (run_single_instance, no_main_window, exit_on_close=false)
 │               surface layer-shell criada no toggle, destruída ao fechar
 ├── watcher.rs— thread própria, conexão Wayland própria, data-control → mpsc
 └── db.rs     — SQLite (WAL); também aberto pela CLI
```

- **Sem IPC proprietário**: o toggle usa `run_single_instance` + D-Bus activation
  (token `XDG_ACTIVATION_TOKEN` repassado pelo libcosmic), exatamente como o
  `cosmic-launcher`. Foco garantido pelo compositor.
- **Alternativas descartadas**: (B) usar `cliphist` como motor — sem pin/FTS/MIME
  ricos e parse textual frágil; (C) forkar `author-clipboard` — GPL-3 e churn de
  terceiros, contra o pedido de construir aqui.
- **Originalidade/licença**: o watcher é implementação própria; nenhum código
  GPL-3 do `cosmic-utils/clipboard-manager` é copiado (usado só como referência de
  quais APIs existem).

## 5. Componentes e interfaces

### 5.1 `main.rs`
- clap com subcomandos acima; `--daemon`/`--toggle` também aceitos como flags
  (o libcosmic exige `CosmicFlags` para o `Args` do `run_single_instance`).
- `key <combo>`: ferramenta de injeção de tecla (`ctrl+v`, `super+v`, …) usando o
  mesmo caminho do auto-paste. Serve para uso avançado **e** é o mecanismo de
  verificação do atalho (seção 12).

### 5.2 `watcher.rs`
```rust
pub struct Captured { pub kind: Kind, pub mime: String, pub text: Option<String>,
                      pub html: Option<String>, pub bytes: Vec<u8>, pub sha256: String }
pub fn spawn(tx: tokio::sync::mpsc::Sender<Captured>) -> anyhow::Result<JoinHandle<()>>;
```
- Conexão Wayland própria (`Connection::connect_to_env`) usando os tipos
  reexportados por `cosmic::cctk` (`cctk::wayland_client`,
  `cctk::sctk::reexports::protocols_wlr::data_control::v1::client`) para não
  duplicar versões de `wayland-client`.
- Preferência de protocolo: `ext_data_control_v1` se disponível, senão
  `zwlr_data_control_manager_v1` (ambos presentes no cosmic-comp).
- Fluxo por seat: `get_data_device` → eventos `data_offer` (coletar MIME types) →
  `selection` (nova seleção) → escolher MIME pela política da seção 7 →
  `std::io::pipe()` + `offer.receive(mime, fd)` → ler com teto de bytes.
- Sem dedupe no watcher: toda nova seleção é emitida. A normalização de conteúdo
  repetido (mesmo `sha256` → sobe pro topo, sem duplicar) é responsabilidade
  exclusiva de `db::upsert` — inclusive quando a seleção foi definida por nós
  mesmos ao escolher um item.
- Erros do watcher são logados e reportados via `status`; o watcher nunca derruba
  a UI.

### 5.3 `db.rs`
```rust
pub struct Db { /* rusqlite::Connection, busy_timeout = 2s */ }
impl Db {
  pub fn open(path: &Path) -> Result<Db>;          // cria schema/migração
  pub fn upsert(&mut self, item: &Captured) -> Result<Upsert>;  // Inserted | Bumped
  pub fn search(&self, query: &str, limit: usize) -> Result<Vec<Item>>;
  pub fn recent(&self, limit: usize) -> Result<Vec<Item>>;
  pub fn set_pinned(&mut self, id: i64, pinned: bool) -> Result<()>;
  pub fn remove(&mut self, id: i64) -> Result<()>;
  pub fn clear(&mut self, keep_pinned: bool) -> Result<usize>;
  pub fn get(&self, id: i64) -> Result<Option<Item>>;
  pub fn stats(&self) -> Result<Stats>;
  fn evict(&mut self, cfg: &Config) -> Result<usize>;
}
```
`rusqlite` com feature `bundled` (mesmo padrão do seu `commandcode-usage`).

### 5.4 `clipboard.rs`
```rust
pub enum Payload<'a> { Text(&'a str), Html{ html:&'a str, plain:&'a str },
                       Image{ mime:&'a str, bytes:&'a [u8] }, Files(&'a str) }
pub fn set(payload: Payload<'_>) -> Result<()>;   // wl-copy --type <mime>, stdin
pub fn classify(mimes: &[String]) -> Option<Kind>;  // função pura, testável
pub fn is_secret_hint(value: Option<&[u8]>) -> bool;  // valor lido de x-kde-passwordManagerHint
```
Seleção é servida por um processo `wl-copy` (comportamento padrão dele: forka e
mantém a seleção). Não implementamos servidor de seleção próprio.

### 5.5 `paste.rs` (auto-paste)
```rust
pub fn send_combo(combo: Combo) -> Result<()>;  // ctrl+v, super+v, …
```
- Conexão Wayland própria; `zwp_virtual_keyboard_manager_v1`
  (`wayland-protocols-misc`); keymap gerado com `xkbcommon` (mmap + `keymap`),
  depois `key`/`modifiers`/`destroy`.
- Usado (a) pelo item 5.1 `key`, (b) após a seleção, quando `auto_paste = true`:
  fecha o popup → espera o foco voltar (delay curto configurável) → envia Ctrl+V.
- Falha → apenas erro logado; o item já está no clipboard.

### 5.6 `shortcut.rs`, `autostart.rs`
- `shortcut::install()`: backup `custom.bak-<epoch>` → insere antes do `}` final
  ```ron
      (
          modifiers: [Super],
          key: "v",
          description: Some("histórico da área de transferência"),
      ): Spawn("/home/pdl/.local/bin/transferencia --toggle"),
  ```
  → escrita atômica (tmp + rename) → re-leitura de conferência. Idempotente
  (detecta `--toggle` já presente e reporta `AlreadyInstalled`).
- `shortcut::uninstall()`: remove exatamente esse bloco.
- `autostart::install()`: escreve
  `~/.config/autostart/com.pdl.Transferencia.desktop` com
  `Exec=/home/pdl/.local/bin/transferencia --daemon`, `X-GNOME-Autostart-enabled=true`.

## 6. Modelo de dados

Arquivo: `~/.local/share/transferencia/history.db` (WAL).

```sql
CREATE TABLE IF NOT EXISTS items (
  id         INTEGER PRIMARY KEY,
  kind       TEXT    NOT NULL CHECK (kind IN ('text','html','image','files')),
  mime       TEXT    NOT NULL,
  text       TEXT,              -- puro (html: companion; files: paths) — base da busca
  html       TEXT,
  blob       BLOB,              -- imagem original
  thumb      BLOB,              -- PNG 256px (render da lista)
  sha256     TEXT    NOT NULL UNIQUE,
  bytes      INTEGER NOT NULL,
  pinned     INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL   -- unix millis
);
CREATE INDEX IF NOT EXISTS idx_items_created ON items(created_at DESC);
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  text, content='items', content_rowid='id', tokenize='unicode61 remove_diacritics 2');
-- triggers AFTER INSERT/DELETE/UPDATE de items mantendo items_fts em sincronia
```

Políticas:
- **Dedupe/upsert**: `sha256` igual → `UPDATE created_at` (vai pro topo), preserva
  `pinned` e `id`.
- **Busca**: vazia → `pinned DESC, created_at DESC`; com texto → `items_fts MATCH`
  (último token com `*`, aspas escapadas), fallback `LIKE` se o MATCH falhar.
- **Retenção** (`evict`, roda após cada upsert): remove os mais antigos **não
  fixados** até respeitar `max_items` e `max_total_bytes`.
- **Imagens**: original em `blob`, thumb 256px em `thumb` (decodificação com a
  crate `image`, já presente na árvore do libcosmic para PNG/JPEG/WebP).
- **Migrações**: tabela `schema_version`; versão 1 = schema acima.

## 7. Política de captura

Ordem de preferência de MIME na oferta:
1. `text/uri-list` → `files`
2. `image/png` → `image/jpeg` → `image/webp` → `image/bmp` → `image/tiff` → `image`
3. `text/html` (+ `text/plain` como companion) → `html`
4. `text/plain;charset=utf-8` → `UTF8_STRING` → `text/plain` → `STRING` → `TEXT` → `text`

Descarte (não grava nada, registra em log de debug):
- oferta contém `x-kde-passwordManagerHint` com valor de bytes contendo `secret`;
- bytes acima de `max_item_bytes`; texto acima de `max_text_bytes`;
- MIME types `chromium/x-*` e afins **não** são conteúdo: só servem de pista, nunca
  viram item.

Tipos de exemplo já vistos nesta máquina (evidência): `text/plain;charset=utf-8`,
`text/html`, `chromium/x-internal-source-rfh-token`, `chromium/x-source-url`.

## 8. UI/UX (popup Super+V)

- Surface layer-shell: layer overlay, **sem âncora** (centralizada),
  `KeyboardInteractivity::Exclusive`, `exclusive_zone = -1`, namespace
  `transferencia`, sem decoração, tamanho do config (padrão 640×440), cantos e
  fundo do tema COSMIC (`LiveSettings` + `theme::Container`).
- Conteúdo: campo de busca no topo (foco automático ao abrir) + lista rolável.
- Linha: preview 1 linha ellipsizado, ícone do tipo (texto/imagem/arquivos/html),
  hora relativa, indicador de fixado.
- Teclas: `↑/↓` navegam, `Enter` cola o item, `Shift+Enter` cola como texto puro,
  `Delete` remove, `Ctrl+P` alterna pino, `Esc` fecha; clique/toque seleciona.
- Seleção: `clipboard::set` → fecha a surface → (se `auto_paste`) Ctrl+V.
- Ao abrir: consulta fresca no DB (a CLI pode ter mudado o histórico).

## 9. Configuração — `~/.config/transferencia/config.toml`

```toml
max_items          = 500
max_total_bytes    = 268435456   # 256 MiB
max_item_bytes     = 20971520    # 20 MiB
max_text_bytes     = 1048576     # 1 MiB
auto_paste         = true
paste_delay_ms     = 150
popup_width        = 640
popup_height       = 440
```
Arquivo ausente → defaults; chave desconhecida → aviso no log, não erro fatal.

## 10. Erros e degradação

| Situação | Comportamento |
|---|---|
| `--toggle` sem daemon rodando | sobe a instância e mostra a janela |
| Compositor sem `data-control` | daemon sobe, popup funciona, `status`/`doctor` reportam "captura indisponível" |
| `wl-copy` ausente | erro explícito ao selecionar (mensagem no `doctor`) |
| `zwp_virtual_keyboard` ausente ou falha | loga, não injeta; item continua copiado |
| Config cosmic ilegível/inesperado | `install` recusa-se a escrever (não corrompe), instrui o caminho pela GUI |
| DB ocupado | `busy_timeout` 2s + erro reportado |
| DB corrompido | `doctor` detecta (`PRAGMA integrity_check`), sugere backup + recriação |

## 11. Verificação

**Testes automatizados (valem a pena — falham em bug plausível):**
- `db`: upsert insere; mesmo sha256 faz bump sem duplicar e preserva `pinned`;
  busca FTS acha por prefixo/substring de token; `evict` respeita `max_items` e
  `max_total_bytes` e nunca remove fixados; `clear --keep-pinned`.
- `clipboard::classify`: ordem de preferência de MIME; uri-list vence imagem;
  hint de senha veta; nenhum MIME conhecido → `None`.
- `clipboard::is_secret_hint`: `x-kde-passwordManagerHint: secret` veta; outros
  valores não vetam.
- `shortcut`: patch insere 1 entrada em arquivo modelo, é idempotente, `uninstall`
  restaura o arquivo original byte a byte.

**Smoke real nesta máquina (prova de entrega):**
1. `cargo test`;
2. daemon + `wl-copy` de texto, imagem e `text/uri-list` → conferir `transferencia list`;
3. abrir o popup (toggle) e **screenshot** confirmando a janela centralizada e a lista;
4. selecionar item → `wl-paste -n` devolve o conteúdo esperado; `--type text/html` para HTML;
5. verificação do atalho sem depender do usuário: `transferencia key super+v` (injeção
   via virtual keyboard) deve abrir o popup; `transferencia key super+Return` deve
   continuar abrindo o alacritty — isso prova que o patch do `custom` **não quebrou**
   o arquivo de atalhos existente;
6. auto-paste: com um campo de texto focado (ex.: editor), selecionar item no popup e
   conferir que o texto apareceu.

## 12. Fases

1. **Fundação** — esqueleto do crate, `config`, `db` + testes, CLI (`list`, `clear`,
   `remove`, `copy`, `status`). Verificação: `cargo test` + `list` lendo um DB semeado.
2. **Captura** — `watcher` + integração no daemon + `doctor`. Verificação: copiar de
   verdade (texto, imagem, arquivo) e ver no `list`.
3. **UI + atalho** — popup libcosmic, single-instance/toggle, `install`
   (atalho+autostart), pins/remoção. Verificação: itens 3–5 do smoke.
4. **Auto-paste + polimento** — `paste`, `auto_paste`, docs de uso e limpeza.
   Verificação: item 6 do smoke.

## 13. Riscos

| Risco | Mitigação |
|---|---|
| Iced/libcosmic sem janela inicial encerrar o loop | seguir o padrão do `cosmic-launcher` (`no_main_window(true)`, `exit_on_close(false)`); se necessário, surface "dummy" 1×1 invisível, como ele faz |
| Versões divergentes de `wayland-client` (cctk vs `wayland-protocols-misc`) | watcher usa tipos via `cosmic::cctk`; o helper de teclado tem conexão própria (duas cópias do crate não conflitam em runtime) |
| Injeção de Ctrl+V cair no alvo errado | delay configurável, só quando `auto_paste = true`, e o item **sempre** fica no clipboard |
| Patch do arquivo de atalhos quebrar o cosmic-config | backup, patch idempotente, teste com Super+Return existente |
| Imagens grandes no DB | thumbs 256px para render, teto por item (`max_item_bytes`) e total (`max_total_bytes`) com evict |

## 14. Referências

- `pop-os/cosmic-launcher` — `src/app.rs` (surface layer-shell, activation, single-instance)
- `pop-os/libcosmic` (rev `87ab817`) — `src/lib.rs` (`pub use cctk`), `Cargo.toml` (features
  `wayland`, `single-instance`, `multi-window`), `src/surface/action.rs`
- `cosmic-utils/clipboard-manager` e `namikofficial/author-clipboard` — prior art
  (apenas referência de APIs; nenhum código copiado)
- Protocolos: `wlr-data-control-unstable-v1`, `ext-data-control-v1`,
  `wlr-layer-shell-unstable-v1`, `zwp-virtual-keyboard-unstable-v1`
