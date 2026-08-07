#!/usr/bin/env sh
set -e

# Everything lives in main() and main is called on the last line, so a
# truncated `curl … | sh` cannot execute a half-downloaded prefix.
main() {
  REPO="h5i-dev/h5i"
  BINARY="h5i"
  INSTALL_DIR="${H5I_INSTALL_DIR:-/usr/local/bin}"

  # ── detect OS ──────────────────────────────────────────────────────────────
  OS="$(uname -s)"
  case "$OS" in
    Linux)  os="linux" ;;
    Darwin) os="macos" ;;
    *)
      echo "Unsupported OS: $OS" >&2
      exit 1
      ;;
  esac

  # ── detect arch ────────────────────────────────────────────────────────────
  ARCH="$(uname -m)"
  case "$ARCH" in
    x86_64 | amd64)  arch="x86_64" ;;
    arm64 | aarch64) arch="aarch64" ;;
    *)
      echo "Unsupported architecture: $ARCH" >&2
      exit 1
      ;;
  esac

  # ── map to release target triple ───────────────────────────────────────────
  case "${os}-${arch}" in
    linux-x86_64)  target="x86_64-unknown-linux-musl" ;;
    linux-aarch64) target="aarch64-unknown-linux-musl" ;;
    macos-aarch64) target="aarch64-apple-darwin" ;;
    # Rosetta 2 translates x86_64 to arm64, not the reverse, so the published
    # Apple Silicon archive cannot run here. Fail before the download rather
    # than install a binary that will not execute.
    macos-x86_64)
      echo "Unsupported platform: macos-x86_64." >&2
      echo "Only Apple Silicon macOS builds are published; Rosetta 2 cannot run a native arm64 binary on an Intel Mac." >&2
      echo "Build from source instead: cargo install --git https://github.com/${REPO}" >&2
      exit 1
      ;;
    # Unreachable while the two cases above cover every os/arch pair, but an
    # unmatched pair would otherwise leave `target` empty and request a
    # nonsense archive URL.
    *)
      echo "Unsupported platform: ${os}-${arch}" >&2
      exit 1
      ;;
  esac

  # ── resolve latest version ─────────────────────────────────────────────────
  VERSION="${H5I_VERSION:-}"
  if [ -z "$VERSION" ]; then
    VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
      | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
  fi

  if [ -z "$VERSION" ]; then
    echo "Could not determine latest version. Set H5I_VERSION=vX.Y.Z to override." >&2
    exit 1
  fi

  # ── download ───────────────────────────────────────────────────────────────
  ARCHIVE="${BINARY}-${VERSION}-${target}.tar.gz"
  URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

  echo "Installing h5i ${VERSION} (${target}) → ${INSTALL_DIR}/${BINARY}"

  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT

  curl -fsSL "$URL" -o "${TMP}/${ARCHIVE}"

  # ── verify against the checksum the release publishes ──────────────────────
  # Not a substitute for signing — same origin as the archive — but it does
  # catch a truncated or corrupted download, and it makes tampering with the
  # asset alone insufficient. H5I_SKIP_CHECKSUM=1 is an explicit, visible
  # escape hatch rather than a silent skip.
  if [ "${H5I_SKIP_CHECKSUM:-0}" = "1" ]; then
    echo "!  checksum verification skipped (H5I_SKIP_CHECKSUM=1)" >&2
  else
    if command -v sha256sum >/dev/null 2>&1; then
      sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
    elif command -v shasum >/dev/null 2>&1; then
      sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
    else
      echo "Neither sha256sum nor shasum found; cannot verify the download." >&2
      echo "Install one, or re-run with H5I_SKIP_CHECKSUM=1 to accept the risk." >&2
      exit 1
    fi

    if ! curl -fsSL "${URL}.sha256" -o "${TMP}/${ARCHIVE}.sha256"; then
      echo "Could not fetch ${ARCHIVE}.sha256 — refusing to install unverified." >&2
      echo "Re-run with H5I_SKIP_CHECKSUM=1 to accept the risk." >&2
      exit 1
    fi

    expected="$(cut -d' ' -f1 < "${TMP}/${ARCHIVE}.sha256")"
    actual="$(sha256_of "${TMP}/${ARCHIVE}")"
    if [ -z "$expected" ] || [ "$expected" != "$actual" ]; then
      echo "Checksum mismatch for ${ARCHIVE}" >&2
      echo "  expected: ${expected:-<empty>}" >&2
      echo "  actual:   ${actual}" >&2
      exit 1
    fi
  fi

  tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"

  # ── install ────────────────────────────────────────────────────────────────
  # `install` rather than `mv`: `mv` preserves the *invoking user's* ownership,
  # which under sudo leaves a user-writable h5i sitting in a root-owned PATH
  # directory. Anything running as that user — including an agent in a
  # workspace-tier box, which shares the uid — could then replace the binary
  # that enforces every box's confinement, and `sudo h5i` would run it as root.
  if [ -w "$INSTALL_DIR" ]; then
    install -m 755 "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
  else
    sudo install -o root -g 0 -m 755 "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
  fi

  echo "✔  h5i ${VERSION} installed — run: h5i --help"
}

main "$@"
