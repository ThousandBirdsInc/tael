#!/bin/sh
# tael installer — downloads a prebuilt binary from the GitHub Release.
#
# Prefers a prebuilt binary because the from-source path compiles a large
# dependency tree and takes minutes; this takes seconds and needs no Rust
# toolchain.
#
#   curl -fsSL https://raw.githubusercontent.com/thousandbirdsinc/tael/main/install.sh | sh
#
# Environment:
#   TAEL_VERSION   version to install (default: latest release)
#   TAEL_INSTALL   directory to install into (default: first writable of
#                  ~/.local/bin, /usr/local/bin)

set -eu

REPO="thousandbirdsinc/tael"
BIN="tael"

fail() {
    echo "install: $*" >&2
    exit 1
}

detect_target() {
    os=$(uname -s)
    arch=$(uname -m)
    case "$os" in
        Linux) os_part="unknown-linux-gnu" ;;
        Darwin) os_part="apple-darwin" ;;
        *)
            # The WAL uses unix-only file I/O, so there is no Windows server
            # build to fall back to. Say so rather than failing obscurely.
            fail "unsupported OS '$os'. tael supports macOS and Linux."
            ;;
    esac
    case "$arch" in
        x86_64 | amd64) arch_part="x86_64" ;;
        arm64 | aarch64) arch_part="aarch64" ;;
        *) fail "unsupported architecture '$arch'" ;;
    esac
    echo "${arch_part}-${os_part}"
}

resolve_version() {
    if [ -n "${TAEL_VERSION:-}" ]; then
        echo "${TAEL_VERSION#v}"
        return
    fi
    # Parsed from the releases API rather than following /latest, so a
    # rate-limited or offline run fails with a clear message instead of
    # downloading an HTML error page.
    version=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null |
        sed -n 's/.*"tag_name": *"v\{0,1\}\([^"]*\)".*/\1/p' | head -n 1)
    [ -n "$version" ] || fail "could not determine the latest version; set TAEL_VERSION to install a specific one"
    echo "$version"
}

install_dir() {
    if [ -n "${TAEL_INSTALL:-}" ]; then
        echo "$TAEL_INSTALL"
        return
    fi
    for candidate in "$HOME/.local/bin" /usr/local/bin; do
        if [ -d "$candidate" ] && [ -w "$candidate" ]; then
            echo "$candidate"
            return
        fi
    done
    # Nothing writable existed; create the per-user one rather than asking for
    # root, which an install script should not need.
    mkdir -p "$HOME/.local/bin"
    echo "$HOME/.local/bin"
}

command -v curl >/dev/null 2>&1 || fail "curl is required"
command -v tar >/dev/null 2>&1 || fail "tar is required"

TARGET=$(detect_target)
VERSION=$(resolve_version)
DEST=$(install_dir)
ARCHIVE="tael-cli-v${VERSION}-${TARGET}.tar.gz"
URL="https://github.com/${REPO}/releases/download/v${VERSION}/${ARCHIVE}"

echo "Installing tael v${VERSION} (${TARGET}) into ${DEST}"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT

curl -fsSL "$URL" -o "$TMP/$ARCHIVE" ||
    fail "download failed: $URL
Check that v${VERSION} has a release for ${TARGET}."

tar -xzf "$TMP/$ARCHIVE" -C "$TMP"
# The archive holds a versioned directory; find the binary rather than
# assuming a layout that a future release could change.
BINPATH=$(find "$TMP" -type f -name "$BIN" -perm -u+x | head -n 1)
[ -n "$BINPATH" ] || fail "no '$BIN' binary found in $ARCHIVE"

mkdir -p "$DEST"
install -m 755 "$BINPATH" "$DEST/$BIN" 2>/dev/null || {
    cp "$BINPATH" "$DEST/$BIN"
    chmod 755 "$DEST/$BIN"
}

echo "Installed $DEST/$BIN"
case ":$PATH:" in
    *":$DEST:"*) ;;
    *) echo "Note: $DEST is not on your PATH. Add it with:"
       echo "  export PATH=\"$DEST:\$PATH\"" ;;
esac

"$DEST/$BIN" --version 2>/dev/null || true
echo
echo "Start the server:  tael serve"
echo "Then, elsewhere:   tael services"
