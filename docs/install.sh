#!/usr/bin/env sh
set -e

# Everything lives in main() and main is called on the last line, so a
# truncated `curl … | sh` cannot execute a half-downloaded prefix.
main() {
  REPO="h5i-dev/h5i"
  INSTALL_DIR="${H5I_INSTALL_DIR:-/usr/local/bin}"

  # ── what to install ────────────────────────────────────────────────────────
  # Two binaries ship from one release, and by default you get both. `h5i` is
  # the front door: `h5i browser open <url>` is the first command anyone runs.
  # `h5i-browser-light` is the engine it launches to render the page.
  #
  # **The default used to be `h5i` alone, and that made the product's first
  # command fail on a fresh install** — `h5i browser open` would report that it
  # could not find an engine. An install that leaves the headline broken is not
  # a smaller install, it is a wrong one.
  #
  # One script rather than two, because almost everything below is shared and
  # security-critical: target mapping, the Rosetta refusal, checksum
  # verification, and the ownership handling that exists because a
  # workspace-tier agent shares the invoking uid. A second copy of that is a
  # second place for it to drift, and the drift would be silent.
  BINARIES="h5i h5i-browser-light"
  for arg in "$@"; do
    case "$arg" in
      # Accepted and does nothing: it is the default now, and quietly rejecting
      # a flag that used to work would break scripts for no gain.
      --with-browser) BINARIES="h5i h5i-browser-light" ;;
      # The engine alone. It runs with no h5i anywhere, enforcing its own
      # allowlist and writing its own fail-closed log, which is worth having in
      # a CI image that renders a page and nothing else. Note what it does not
      # give you: `h5i browser` is the surface that knows about session names,
      # placement, the control lock and the audit, and none of that is here.
      --browser-only) BINARIES="h5i-browser-light" ;;
      # h5i without a browser. For a machine that only makes boxes.
      --no-browser) BINARIES="h5i" ;;
      -h | --help)
        echo "Usage: install.sh [--browser-only | --no-browser]"
        echo
        echo "  (no flag)        install h5i and the h5i-browser-light engine"
        echo "  --browser-only   install only the engine, without h5i"
        echo "  --no-browser     install only h5i, without the engine"
        echo
        echo "Environment: H5I_INSTALL_DIR, H5I_VERSION, H5I_SKIP_CHECKSUM"
        exit 0
        ;;
      *)
        echo "Unknown option: $arg" >&2
        echo "Try: install.sh --help" >&2
        exit 1
        ;;
    esac
  done

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

  TMP="$(mktemp -d)"
  trap 'rm -rf "$TMP"' EXIT

  # ── the checksum helper, resolved once ─────────────────────────────────────
  # Hoisted out of the per-binary loop: whether this machine has a sha256 tool
  # is a property of the machine, and asking twice would let a two-binary
  # install fail halfway with the first already on the PATH.
  if [ "${H5I_SKIP_CHECKSUM:-0}" = "1" ]; then
    echo "!  checksum verification skipped (H5I_SKIP_CHECKSUM=1)" >&2
  elif command -v sha256sum >/dev/null 2>&1; then
    sha256_of() { sha256sum "$1" | cut -d' ' -f1; }
  elif command -v shasum >/dev/null 2>&1; then
    sha256_of() { shasum -a 256 "$1" | cut -d' ' -f1; }
  else
    echo "Neither sha256sum nor shasum found; cannot verify the download." >&2
    echo "Install one, or re-run with H5I_SKIP_CHECKSUM=1 to accept the risk." >&2
    exit 1
  fi

  for BINARY in $BINARIES; do
    # ── download ───────────────────────────────────────────────────────────────
    ARCHIVE="${BINARY}-${VERSION}-${target}.tar.gz"
    URL="https://github.com/${REPO}/releases/download/${VERSION}/${ARCHIVE}"

    echo "Installing ${BINARY} ${VERSION} (${target}) → ${INSTALL_DIR}/${BINARY}"

    if ! curl -fsSL "$URL" -o "${TMP}/${ARCHIVE}"; then
      echo "Could not download ${ARCHIVE}." >&2
      echo "  ${URL}" >&2
      echo "Releases before the one that added ${BINARY} do not carry it; pick a newer" >&2
      echo "H5I_VERSION, or check the asset list on the releases page." >&2
      exit 1
    fi

    # ── verify against the checksum the release publishes ──────────────────────
    # Not a substitute for signing — same origin as the archive — but it does
    # catch a truncated or corrupted download, and it makes tampering with the
    # asset alone insufficient. H5I_SKIP_CHECKSUM=1 is an explicit, visible
    # escape hatch rather than a silent skip.
    if [ "${H5I_SKIP_CHECKSUM:-0}" != "1" ]; then
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
      # The ownership fix above only helps on the sudo branch. Here the
      # *directory* is writable by this user, so the file's owner and mode are
      # beside the point: anything running as this user can replace or unlink the
      # binary whatever it is set to — and on a Homebrew macOS, a user-owned
      # /usr/local/bin is the default rather than the exception.
      #
      # That is the same actor the comment above is about. An agent in a
      # workspace-tier box shares this uid by design, so it can rewrite the
      # binary that enforces every other box's confinement, and `sudo h5i` would
      # then run it as root. h5i cannot fix this from inside an install script —
      # where the operator keeps their binaries is theirs to decide — so it says
      # so rather than leaving it to be discovered.
      echo "!  ${INSTALL_DIR} is writable by this user, so anything running as you can replace" >&2
      echo "   ${INSTALL_DIR}/${BINARY} — including an agent in an isolation=workspace box, which" >&2
      echo "   shares your uid. For a root-owned install: H5I_INSTALL_DIR=/opt/h5i/bin sh install.sh" >&2
    else
      sudo install -o root -g 0 -m 755 "${TMP}/${BINARY}" "${INSTALL_DIR}/${BINARY}"
    fi

    echo "✔  ${BINARY} ${VERSION} installed: run ${BINARY} --help"
  done
}

main "$@"
