#!/bin/sh
# Pawrly installer — downloads a prebuilt `pawrly` binary for your platform and
# installs it onto your PATH.
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/CITGuru/pawrly/main/scripts/install.sh | sh
#
# Environment overrides:
#   PAWRLY_VERSION      Tag to install (e.g. v0.1.0). Default: latest release.
#   PAWRLY_INSTALL_DIR  Directory to install the binary into.
#                       Default: $HOME/.local/bin (falls back to /usr/local/bin
#                       when writable, prompting for sudo only if needed).
#   PAWRLY_REPO         owner/repo to pull releases from. Default: CITGuru/pawrly.
#   PAWRLY_NO_VERIFY    Set to 1 to skip the SHA-256 checksum verification.
#   PAWRLY_FORCE        Set to 1 to reinstall even if the target version is
#                       already installed.
#   PAWRLY_BUILD_FROM_SOURCE
#                       Set to 1 to skip prebuilt binaries and `cargo install` instead.
#
# Re-running this script upgrades an existing install, skipping the download
# when already up to date (unless PAWRLY_FORCE=1).
#
# This script is intentionally POSIX sh (no bashisms) so it runs anywhere
# `curl ... | sh` lands.

set -eu

REPO="${PAWRLY_REPO:-CITGuru/pawrly}"
BIN_NAME="pawrly"
# The crate that produces the `pawrly` binary (the `pawrly` package is the library).
CARGO_PKG="pawrly-cli"

# ----- pretty output ---------------------------------------------------------

# Colors only when stderr is a TTY.
if [ -t 2 ]; then
  BOLD="$(printf '\033[1m')"
  RED="$(printf '\033[31m')"
  GREEN="$(printf '\033[32m')"
  YELLOW="$(printf '\033[33m')"
  RESET="$(printf '\033[0m')"
else
  BOLD="" RED="" GREEN="" YELLOW="" RESET=""
fi

info()  { printf '%s\n' "${BOLD}pawrly${RESET} $*" >&2; }
warn()  { printf '%s\n' "${YELLOW}warning:${RESET} $*" >&2; }
err()   { printf '%s\n' "${RED}error:${RESET} $*" >&2; }
die()   { err "$*"; exit 1; }

need_cmd() {
  command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

# ----- download helpers ------------------------------------------------------

# Pick a downloader once.
if command -v curl >/dev/null 2>&1; then
  DL="curl"
elif command -v wget >/dev/null 2>&1; then
  DL="wget"
else
  die "need either curl or wget installed to download Pawrly"
fi

# fetch <url> <output-file>
fetch() {
  _url="$1"; _out="$2"
  if [ "$DL" = "curl" ]; then
    curl -fsSL --proto '=https' --tlsv1.2 "$_url" -o "$_out"
  else
    wget -q "$_url" -O "$_out"
  fi
}

# fetch_stdout <url>
fetch_stdout() {
  if [ "$DL" = "curl" ]; then
    curl -fsSL --proto '=https' --tlsv1.2 "$1"
  else
    wget -q "$1" -O -
  fi
}

# ----- platform detection ----------------------------------------------------

detect_target() {
  _os="$(uname -s)"
  _arch="$(uname -m)"

  case "$_os" in
    Linux)  _os_part="unknown-linux-gnu" ;;
    Darwin) _os_part="apple-darwin" ;;
    *) die "unsupported OS: $_os (Pawrly ships prebuilt binaries for Linux and macOS; set PAWRLY_BUILD_FROM_SOURCE=1 to build instead)" ;;
  esac

  case "$_arch" in
    x86_64 | amd64)        _arch_part="x86_64" ;;
    arm64 | aarch64)       _arch_part="aarch64" ;;
    *) die "unsupported architecture: $_arch" ;;
  esac

  printf '%s-%s' "$_arch_part" "$_os_part"
}

# ----- version resolution ----------------------------------------------------

latest_version() {
  # Resolve the latest release tag via the GitHub API, without requiring jq.
  _api="https://api.github.com/repos/${REPO}/releases/latest"
  _tag="$(fetch_stdout "$_api" \
    | grep -m1 '"tag_name"' \
    | sed -E 's/.*"tag_name"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/')"
  [ -n "$_tag" ] || die "could not determine the latest release from $_api"
  printf '%s' "$_tag"
}

# ----- install dir resolution ------------------------------------------------

resolve_install_dir() {
  if [ -n "${PAWRLY_INSTALL_DIR:-}" ]; then
    printf '%s' "$PAWRLY_INSTALL_DIR"
    return
  fi
  # Prefer a user-writable dir that needs no sudo.
  printf '%s' "$HOME/.local/bin"
}

# ----- existing-install detection --------------------------------------------

# installed_version <install-dir>
# Print the version of an already-installed pawrly (the binary in the target
# dir, else whatever is first on PATH), or nothing if none is found. The CLI
# prints `pawrly <version>` to stdout for `--version`.
installed_version() {
  _dir="$1"
  _bin=""
  if [ -x "$_dir/$BIN_NAME" ]; then
    _bin="$_dir/$BIN_NAME"
  elif command -v "$BIN_NAME" >/dev/null 2>&1; then
    _bin="$(command -v "$BIN_NAME")"
  fi
  [ -n "$_bin" ] || return 0
  "$_bin" --version 2>/dev/null | awk 'NR==1 {print $2}'
}

# ----- build-from-source fallback -------------------------------------------

install_from_source() {
  info "building from source with cargo"
  command -v cargo >/dev/null 2>&1 || \
    die "cargo not found — install Rust from https://rustup.rs first"
  _ref="${PAWRLY_VERSION:-}"
  if [ -n "$_ref" ]; then
    cargo install --git "https://github.com/${REPO}" --tag "$_ref" "$CARGO_PKG"
  else
    cargo install --git "https://github.com/${REPO}" "$CARGO_PKG"
  fi
  info "${GREEN}installed${RESET} via cargo (binary in \$HOME/.cargo/bin)"
  exit 0
}

# ----- main ------------------------------------------------------------------

main() {
  if [ "${PAWRLY_BUILD_FROM_SOURCE:-0}" = "1" ]; then
    install_from_source
  fi

  target="$(detect_target)"
  version="${PAWRLY_VERSION:-$(latest_version)}"
  install_dir="$(resolve_install_dir)"

  # Skip the download when the target version is already installed; otherwise
  # upgrade in place. Strip the tag's leading `v` so `v0.1.0` matches the CLI's
  # `0.1.0`.
  target_ver="${version#v}"
  current_ver="$(installed_version "$install_dir")"
  action="installing"
  if [ -n "$current_ver" ]; then
    if [ "$current_ver" = "$target_ver" ]; then
      if [ "${PAWRLY_FORCE:-0}" = "1" ]; then
        action="reinstalling"
      else
        info "pawrly ${BOLD}v${current_ver}${RESET} is already up to date"
        info "set ${BOLD}PAWRLY_FORCE=1${RESET} to reinstall"
        exit 0
      fi
    else
      action="updating"
    fi
  fi

  tarball="${BIN_NAME}-${target}.tar.gz"
  base_url="https://github.com/${REPO}/releases/download/${version}"
  url="${base_url}/${tarball}"
  sum_url="${url}.sha256"

  if [ "$action" = "updating" ]; then
    info "updating ${BOLD}pawrly${RESET} v${current_ver} → ${BOLD}${version}${RESET} for ${BOLD}${target}${RESET}"
  else
    info "${action} ${BOLD}${version}${RESET} for ${BOLD}${target}${RESET}"
  fi

  tmp="$(mktemp -d 2>/dev/null || mktemp -d -t pawrly)"
  # shellcheck disable=SC2064
  trap "rm -rf \"$tmp\"" EXIT INT TERM

  info "downloading ${url}"
  if ! fetch "$url" "$tmp/$tarball"; then
    warn "no prebuilt binary found for ${target} at ${version}"
    install_from_source
  fi

  # Verify checksum unless explicitly disabled.
  if [ "${PAWRLY_NO_VERIFY:-0}" != "1" ]; then
    if fetch "$sum_url" "$tmp/$tarball.sha256" 2>/dev/null; then
      verify_checksum "$tmp/$tarball" "$tmp/$tarball.sha256"
    else
      warn "no checksum published for ${tarball}; skipping verification"
    fi
  fi

  info "extracting"
  tar -xzf "$tmp/$tarball" -C "$tmp"
  [ -f "$tmp/$BIN_NAME" ] || die "archive did not contain a '$BIN_NAME' binary"
  chmod +x "$tmp/$BIN_NAME"

  place_binary "$tmp/$BIN_NAME" "$install_dir"

  installed="$install_dir/$BIN_NAME"
  info "${GREEN}installed${RESET} $installed"
  "$installed" --version 2>/dev/null || true

  case ":$PATH:" in
    *":$install_dir:"*) ;;
    *) print_path_hint "$install_dir" ;;
  esac
}

verify_checksum() {
  _file="$1"; _sumfile="$2"
  # The sidecar file is `<sha256>  <filename>` as produced by sha256sum.
  _expected="$(awk '{print $1}' "$_sumfile" | head -n1)"
  [ -n "$_expected" ] || { warn "empty checksum file; skipping verification"; return; }

  if command -v sha256sum >/dev/null 2>&1; then
    _actual="$(sha256sum "$_file" | awk '{print $1}')"
  elif command -v shasum >/dev/null 2>&1; then
    _actual="$(shasum -a 256 "$_file" | awk '{print $1}')"
  else
    warn "no sha256sum/shasum available; skipping verification"
    return
  fi

  if [ "$_expected" != "$_actual" ]; then
    die "checksum mismatch — expected $_expected, got $_actual"
  fi
  info "checksum verified"
}

place_binary() {
  _src="$1"; _dir="$2"
  if [ -d "$_dir" ] || mkdir -p "$_dir" 2>/dev/null; then
    if [ -w "$_dir" ]; then
      mv -f "$_src" "$_dir/$BIN_NAME"
      return
    fi
  fi
  # Need elevated privileges.
  if command -v sudo >/dev/null 2>&1; then
    warn "$_dir is not writable; using sudo"
    sudo mkdir -p "$_dir"
    sudo mv -f "$_src" "$_dir/$BIN_NAME"
  else
    die "cannot write to $_dir and sudo is unavailable — set PAWRLY_INSTALL_DIR to a writable directory"
  fi
}

print_path_hint() {
  _dir="$1"
  warn "$_dir is not on your PATH"
  _shell="$(basename "${SHELL:-sh}")"
  case "$_shell" in
    fish) _rc="~/.config/fish/config.fish"; _line="fish_add_path $_dir" ;;
    zsh)  _rc="~/.zshrc";  _line="export PATH=\"$_dir:\$PATH\"" ;;
    *)    _rc="~/.bashrc"; _line="export PATH=\"$_dir:\$PATH\"" ;;
  esac
  printf '%s\n' "  Add it by appending this to ${_rc}:" >&2
  printf '%s\n' "    ${BOLD}${_line}${RESET}" >&2
}

main "$@"
