# transferencia

Histórico da área de transferência nativo para o COSMIC: **Super+V** abre uma lista
centralizada dos itens copiados, com busca, navegação por teclado, pins e colagem
direta no aplicativo em foco.

Escrito em Rust. Sem daemon de terceiros, sem serviço externo, sem rede.

## Requisitos

- COSMIC (testado no 1.8.0). O compositor precisa expor
  `zwlr_data_control_manager_v1`, `zwlr_layer_shell_v1` e
  `zwp_virtual_keyboard_manager_v1` — o `transferencia doctor` confere.
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

| Ação | Resultado |
|---|---|
| 1 clique numa linha | seleciona e mostra a **prévia à direita** (não cola) |
| 2 cliques na linha | cola o item e fecha |
| clique fora do painel | fecha |
| `Super+V` | abre/fecha o popup |
| `↑` `↓` (ou `Tab`) | navega |
| `Enter` | coloca o item no clipboard e cola no aplicativo anterior |
| `Shift+Enter` | cola como texto puro (ignora a marcação de itens HTML) |
| `Ctrl+P` | fixa/desfixa (itens fixados não saem pelo limite) |
| `Delete` | remove o item |
| `PageUp` `PageDown` | rolam o painel de prévia (textos grandes) |
| `Esc` | fecha |

Itens HTML vão para o clipboard com `text/html` (a marcação) **e** `text/plain` (o
texto): aplicativos que não entendem HTML — campo de texto, terminal, editor
simples — recebem o texto limpo, não a marcação.

O painel direito mostra a prévia do item selecionado: imagens em tamanho grande,
texto/arquivos como conteúdo. A prévia acompanha as setas do teclado. Textos
longos rolam com a roda do mouse, com a barra de rolagem ou com `PageUp`/`PageDown`
(até 60 000 caracteres renderizados; acima disso o painel avisa quanto ficou fora).

## CLI

```sh
transferencia list [--limit N] [--json]     # itens recentes
transferencia search <termo>                # busca FTS (sem acento, por prefixo)
transferencia copy <id> [--plain]           # devolve um item ao clipboard (fica um processo servindo)
transferencia pin <id> | unpin <id>
transferencia remove <id>
transferencia clear [--keep-pinned] --yes
transferencia status [--json]               # estado do banco e da configuração
transferencia doctor                        # checa protocolos, seleção, teclado virtual, atalho
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
popup_width        = 1000
popup_height       = 460
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
- **Popup**: instância única por ativação D-Bus. A janela é uma layer surface de
  **tela cheia transparente**: o painel fica centralizado, o blur do tema é
  aplicado só no retângulo do painel e o resto da superfície captura o clique de
  fora (que fecha). Como a superfície cobre a saída, o ponteiro nunca sai dela —
  o compositor não devolve o foco para outra janela no meio do uso do mouse.
- **Aparência**: segue o tema do COSMIC, incluindo o modo vidro
  (`frosted_system_interface`): fundo em `background(theme.transparent)` e blur
  atrás do painel.
- **Colagem**: a seleção é publicada pelo próprio transferencia
  (`zwlr_data_control_manager_v1`), com um conteúdo por tipo MIME — item HTML
  oferece a marcação em `text/html` e o texto em `text/plain`. Com `auto_paste`, um
  Ctrl+V é injetado pelo teclado virtual do compositor depois que o popup fecha.
  Se a injeção falhar, o item continua no clipboard — é só colar à mão. No popup
  quem serve é o daemon; no `transferencia copy`, um processo em segundo plano, que
  sai quando outro aplicativo copia algo (a seleção morre com quem a publica).
- **Imagens**: miniatura de 256 px (lista) e prévia de 1024 px (painel direito) no
  banco; o original em BLOB é o que vai para o clipboard. Imagens capturadas antes
  do painel de prévia ganham a prévia na primeira vez que o daemon sobe.

## Dados

- Banco: `~/.local/share/transferencia/history.db` (SQLite em WAL, seguro para
  vários processos: o daemon escreve, a CLI lê).
- Config: `~/.config/transferencia/config.toml`
- Autostart: `~/.config/autostart/com.pdl.Transferencia.desktop`

## Limites conhecidos

- Só o clipboard normal; a seleção primária (X11) não é observada.
- O auto-paste injeta `Ctrl+V`; terminais costumam usar `Ctrl+Shift+V` — neles o
  atalho do terminal cola, ou desligue `auto_paste` e cole à mão.
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
