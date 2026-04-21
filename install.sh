#!/usr/bin/env sh
set -e

REPO="Koukyosyumei/h5i"
BINARY="h5i"
INSTALL_DIR="${H5I_INSTALL_DIR:-/usr/local/bin}"

# ── detect OS ────────────────────────────────────────────────────────────────
OS="$(uname -s)"
case "$OS" in
  Linux)  os="linux" ;;
  Darwin) os="macos" ;;
  *)
    echo "Unsupported OS: $OS" >&2
    exit 1
    ;;
esac

# ── detect arch ──────────────────────────────────────────────────────────────
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64 | amd64)  arch="x86_64" ;;
  arm64 | aarch64) arch="aarch64" ;;
  *)
    echo "Unsupported architecture: $ARCH" >&2
    exit 1
    ;;
esac

# ── map to release target triple ─────────────────────────────────────────────
case "${os}-${arch}" in
  linux-x86_64)  target="x86_64-unknown-linux-musl" ;;
  linux-aarch64) target="aarch64-unknown-linux-musl" ;;
  macos-x86_64 | macos-aarch64) target="aarch64-apple-darwin" ;;
esac

# ── resolve latest version ───────────────────────────────────────────────────
VERSION="${H5I_VERSION:-}"
if [ -z "$VERSION" ]; then
  VERSION="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
    | grep '"tag_name"' | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/')"
fi

if [ -z "$VERSION" ]; then
  echo "Could not determine latest version. Set H5I_VERSION=vX.Y.Z to override." >&2
  exit 1
fi

# ── download and install ─────────────────────────────────────────────────────
ARCHIVE="${BINARY}-${VERSION}-${target}.tar.gz"
URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

echo "Installing h5i ${VERSION} (${target}) → ${INSTALL_DIR}/${BINARY}"

TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "${TMP}/${ARCHIVE}"
tar -xzf "${TMP}/${ARCHIVE}" -C "$TMP"

if [ -w "$INSTALL_DIR" ]; then
  mv "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
else
  sudo mv "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
fi

echo "✔  h5i ${VERSION} installed — run: h5i --help"
