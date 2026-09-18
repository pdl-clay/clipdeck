# transferencia

Histórico da área de transferência nativo para o COSMIC: **Super+V** abre uma lista
centralizada dos itens copiados, com busca, navegação por teclado, pins e colagem
direta no aplicativo em foco.

Escrito em Rust. Sem daemon de terceiros, sem serviço externo, sem rede.

## Requisitos

- COSMIC (testado no 1.8.0). O compositor precisa expor
  `zwlr_data_control_manager_v1`, `zwlr_layer_shell_v1` e
  `zwp_virtual_keyboard_manager_v1` — o `transferencia doctor` confere.
- `wl-copy` (pacote `wl-clipboard`) para servir a seleção.
- Rust 1.93+ só para compilar.

## Instalação

```sh
cargo build --release
install -Dm755 target/release/transferencia ~/.local/bin/transferencia
transferencia install
```

`install` faz três coisas: grava o autostart do daemon
(`~/.config/autostart/com.pdl.Transferencia.desktop`), adiciona o atalho Super+V no
arquivo de atalhos do COSMIC (com backup `custom.bak-<epoch>` ao lado) e sobe o
daemon agora, sem esperar logout.

Se preferir não deixar o programa mexer no arquivo de atalhos, adicione à mão em
**Ajustes → Teclado → Atalhos personalizados**: comando `transferencia --toggle`,
tecla `Super+V`.

## Uso

| Tecla | Ação |
|---|---|
| `Super+V` | abre/fecha o popup |
| `↑` `↓` (ou `Tab`) | navega |
| `Enter` | coloca o item no clipboard e cola no aplicativo anterior |
| `Shift+Enter` | cola como texto puro (útil para itens HTML) |
| `Ctrl+P` | fixa/desfixa (itens fixados não saem pelo limite) |
| `Delete` | remove o item |
| `Esc` | fecha |

Clique também seleciona um item. O campo de busca tem foco automático. Perder o
foco fecha o popup — mesmo comportamento do `cosmic-launcher` (clicar em outra
janela fecha a lista).

## CLI

```sh
transferencia list [--limit N] [--json]     # itens recentes
transferencia search <termo>                # busca FTS (sem acento, por prefixo)
transferencia copy <id> [--plain]           # devolve um item ao clipboard
transferencia pin <id> | unpin <id>
transferencia remove <id>
transferencia clear [--keep-pinned] --yes
transferencia status [--json]               # estado do banco e da configuração
transferencia doctor                        # checa protocolos, wl-copy, teclado virtual, atalho
transferencia key ctrl+v | super+v | up | down | enter | escape | delete | tab
transferencia install | uninstall [--purge]
```

## Configuração — `~/.config/transferencia/config.toml`

```toml
max_items          = 500          # itens no histórico (fixados não contam)
max_total_bytes    = 268435456    # teto somando tudo (256 MiB)
max_item_bytes     = 20971520     # teto por item (20 MiB)
max_text_bytes     = 1048576      # teto para texto (1 MiB)
auto_paste         = true         # colar automaticamente após escolher
paste_delay_ms     = 150          # espera para o foco voltar ao app anterior
popup_width        = 640
popup_height       = 440
```

O arquivo é opcional: ausente, valem os padrões acima. Ajustes de popup e de
colagem passam a valer na próxima abertura do popup; os limites de captura
(`max_*`) valem a partir do próximo início do daemon.

## Como funciona

- **Captura**: thread própria conectada ao Wayland via `zwlr_data_control_manager_v1`
  (observa o clipboard sem precisar de foco). A cada nova seleção escolhe o melhor
  tipo MIME disponível — `text/uri-list` → imagem → `text/html` (com o
  `text/plain` companheiro) → texto — e grava no SQLite.
- **Dedupe**: SHA-256 do conteúdo. Copiar de novo o mesmo conteúdo sobe o item para
  o topo em vez de duplicar.
- **Privacidade**: ofertas marcadas com `x-kde-passwordManagerHint: secret`
  (gerenciadores de senha) não são gravadas. Nada sai da máquina.
- **Popup**: instância única por ativação D-Bus; a janela é uma layer surface
  centralizada (mesmo mecanismo do `cosmic-launcher`), criada no `Super+V` e
  destruída ao fechar.
- **Colagem**: o item vai para o clipboard por `wl-copy`; com `auto_paste`, um
  Ctrl+V é injetado pelo teclado virtual do compositor depois que o popup fecha.
  Se a injeção falhar, o item continua no clipboard — é só colar à mão.
- **Imagens**: miniatura de 256 px guardada no banco (render da lista) e o original
  em BLOB (usado na colagem).

## Dados

- Banco: `~/.local/share/transferencia/history.db` (SQLite em WAL, seguro para
  vários processos: o daemon escreve, a CLI lê).
- Config: `~/.config/transferencia/config.toml`
- Autostart: `~/.config/autostart/com.pdl.Transferencia.desktop`

## Limites conhecidos

- Só o clipboard normal; a seleção primária (X11) não é observada.
- O auto-paste depende de o foco voltar ao aplicativo anterior; o `paste_delay_ms`
  ajusta a espera para ambientes mais lentos.
- Aplicativos que servem a seleção e morrem em seguida podem entregar conteúdo
  vazio; nesse caso a cópia é ignorada (o `wl-clip-persist` resolve do lado do
  sistema, se for o caso).

## Desinstalar

```sh
transferencia uninstall            # remove autostart e atalho, mantém o histórico
transferencia uninstall --purge    # remove também o banco
```
