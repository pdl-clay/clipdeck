#!/usr/bin/env bash
#
# clipdeck installer
#
# Baixa o binário do último release, confere o checksum, instala em
# ~/.local/bin e registra o autostart + o atalho Super+V.
#
#   curl -fsSL https://raw.githubusercontent.com/pdl-clay/clipdeck/main/install.sh | bash
#
set -euo pipefail

REPO="pdl-clay/clipdeck"
BIN_NAME="clipdeck"

PREFIX="${HOME}/.local"
BINDIR="${PREFIX}/bin"
WANT_VERSION=""
DO_SETUP=1
ASSUME_YES=0

c_dim=$'\033[2m'; c_ok=$'\033[32m'; c_err=$'\033[31m'; c_hi=$'\033[1m'; c_off=$'\033[0m'
[ -t 1 ] || { c_dim=""; c_ok=""; c_err=""; c_hi=""; c_off=""; }

die()  { printf '%serror:%s %s\n' "$c_err" "$c_off" "$1" >&2; exit 1; }
info() { printf '%s==>%s %s\n' "$c_dim" "$c_off" "$1"; }
ok()   { printf '  %s✓%s %s\n' "$c_ok" "$c_off" "$1"; }

usage() {
  cat <<EOF
${c_hi}clipdeck${c_off} — clipboard history for COSMIC

  curl -fsSL https://raw.githubusercontent.com/${REPO}/main/install.sh | bash

${c_hi}Options${c_off}
  --version <tag>   instala uma versão específica (padrão: último release)
  --prefix <dir>    onde instalar (padrão: ~/.local)
  --no-setup        não registra autostart nem o atalho Super+V
  --yes, -y         não pede confirmação
  --help, -h        esta ajuda
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --version) WANT_VERSION="${2:?--version precisa de um valor}"; shift 2 ;;
    --prefix)  PREFIX="${2:?--prefix precisa de um valor}";      shift 2 ;;
    --no-setup) DO_SETUP=0; shift ;;
    --yes|-y)  ASSUME_YES=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) die "opção desconhecida: $1 (tente --help)" ;;
  esac
done

BINDIR="${PREFIX}/bin"

command -v curl >/dev/null 2>&1 || die "curl é necessário"
command -v tar  >/dev/null 2>&1 || die "tar é necessário"

# --- arch ------------------------------------------------------------------
case "$(uname -m)" in
  x86_64|amd64)  ARCH="x86_64" ;;
  aarch64|arm64) ARCH="aarch64" ;;
  *) die "arquitetura não suportada: $(uname -m) — compile do fonte com 'cargo install --git https://github.com/${REPO}'" ;;
esac

OS="$(uname -s)"
[ "$OS" = "Linux" ] || die "clipdeck só roda em Linux (detectado: $OS)"

# --- versão ----------------------------------------------------------------
if [ -z "$WANT_VERSION" ]; then
  info "procurando o último release"
  URL_LATEST="https://github.com/${REPO}/releases/latest"
  EFFECTIVE="$(curl -fsSL -o /dev/null -w '%{url_effective}' "$URL_LATEST" \
    || die "não consegui consultar o último release")"
  WANT_VERSION="${EFFECTIVE##*/}"
  [ -n "$WANT_VERSION" ] || die "não consegui determinar a versão mais recente"
fi

TARBALL="${BIN_NAME}-${WANT_VERSION}-linux-${ARCH}.tar.gz"
BASE="https://github.com/${REPO}/releases/download/${WANT_VERSION}"
SHASUMS="${BIN_NAME}-${WANT_VERSION}-checksums.txt"

printf '\n%s%s%s %s(%s)%s\n\n' "$c_hi" "$BIN_NAME" "$c_off" "$c_dim" "$WANT_VERSION" "$c_off"

# --- download --------------------------------------------------------------
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

info "baixando ${c_hi}${TARBALL}${c_off}"
curl -fL --progress-bar -o "${TMP}/${TARBALL}" "${BASE}/${TARBALL}" \
  || die "falha no download — o release ${WANT_VERSION} tem artefato para linux/${ARCH}?"
ok "baixado"

# --- checksum --------------------------------------------------------------
if curl -fsSL -o "${TMP}/${SHASUMS}" "${BASE}/${SHASUMS}" 2>/dev/null; then
  EXPECTED="$(awk -v f="$TARBALL" '$2 == f || $2 == "*"f {print $1; exit}' "${TMP}/${SHASUMS}")"
  if [ -n "$EXPECTED" ]; then
    ACTUAL="$(sha256sum "${TMP}/${TARBALL}" | awk '{print $1}')"
    [ "$EXPECTED" = "$ACTUAL" ] || die "checksum não confere (esperado ${EXPECTED}, obtido ${ACTUAL})"
    ok "checksum confere"
  else
    printf '  %s!%s %s não lista %s — seguindo sem conferir\n' \
      "$c_dim" "$c_off" "$SHASUMS" "$TARBALL"
  fi
else
  printf '  %s!%s checksums não disponível neste release — seguindo sem conferir\n' "$c_dim" "$c_off"
fi

# --- extrai e instala ------------------------------------------------------
tar -xzf "${TMP}/${TARBALL}" -C "$TMP"
[ -f "${TMP}/${BIN_NAME}" ] || die "o tarball não trouxe o executável ${BIN_NAME}"

if [ "$ASSUME_YES" -eq 0 ] && [ -t 0 ]; then
  printf '\nInstalar %s em %s? [Y/n] ' "$BIN_NAME" "$BINDIR"
  read -r reply
  case "$reply" in [Nn]*) die "cancelado" ;; esac
fi

mkdir -p "$BINDIR"
# Instala por rename para nunca deixar um binário meio escrito no PATH.
install -m 0755 "${TMP}/${BIN_NAME}" "${BINDIR}/.${BIN_NAME}.new"
mv -f "${BINDIR}/.${BIN_NAME}.new" "${BINDIR}/${BIN_NAME}"
ok "instalado em ${BINDIR}/${BIN_NAME}"

if [[ ":${PATH}:" != *":${BINDIR}:"* ]]; then
  # O $PATH na linha abaixo é impresso literalmente: é a linha que o usuário copia
  # para o shell dele, não algo para expandir aqui.
  # shellcheck disable=SC2016
  printf '  %s!%s %s não está no seu PATH — adicione:\n     export PATH="%s:$PATH"\n' \
    "$c_dim" "$c_off" "$BINDIR" "$BINDIR"
fi

# --- autostart + atalho ----------------------------------------------------
if [ "$DO_SETUP" -eq 1 ]; then
  if [ -n "${WAYLAND_DISPLAY:-}" ]; then
    info "registrando autostart e o atalho Super+V"
    if "${BINDIR}/${BIN_NAME}" install; then
      ok "pronto — Super+V abre o histórico"
    else
      printf '  %s!%s não consegui registrar; rode %s install manualmente\n' \
        "$c_dim" "$c_off" "${BINDIR}/${BIN_NAME}"
    fi
  else
    printf '  %s!%s sem WAYLAND_DISPLAY (fora de uma sessão gráfica?) — pulei o setup.\n' "$c_dim" "$c_off"
    printf '     Entre no COSMIC e rode: %s install\n' "${BINDIR}/${BIN_NAME}"
  fi
else
  printf '  %s(%s setup pulado — rode %s install quando quiser%s%s)\n' \
    "$c_dim" "$BIN_NAME" "$BIN_NAME" "$c_off" "$c_dim"
fi

cat <<EOF

${c_hi}Pronto.${c_off} Copie alguma coisa e aperte ${c_hi}Super+V${c_off}.
  ${BIN_NAME} doctor   # diagnostica protocolos, atalho e teclado virtual
  ${BIN_NAME} status   # estado do banco
  ${BIN_NAME} --help   # todos os comandos

${c_dim}Desinstalar: ${BIN_NAME} uninstall --purge${c_off}
EOF