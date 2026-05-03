#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Package a single-binary CLI as a Debian archive.

Usage:
  package_deb.sh --binary PATH --name NAME --version X.Y.Z --arch amd64 --out-dir dist --desc TEXT --homepage URL
EOF
}

BINARY_PATH=""
PACKAGE_NAME=""
VERSION=""
ARCH=""
OUT_DIR=""
DESC=""
HOMEPAGE=""
MAINTAINER="Daulet <noreply@github.com>"
DEPENDS=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary)
      BINARY_PATH="$2"
      shift 2
      ;;
    --name)
      PACKAGE_NAME="$2"
      shift 2
      ;;
    --version)
      VERSION="$2"
      shift 2
      ;;
    --arch)
      ARCH="$2"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="$2"
      shift 2
      ;;
    --desc)
      DESC="$2"
      shift 2
      ;;
    --homepage)
      HOMEPAGE="$2"
      shift 2
      ;;
    --maintainer)
      MAINTAINER="$2"
      shift 2
      ;;
    --depends)
      DEPENDS="$2"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [[ -z "${BINARY_PATH}" || -z "${PACKAGE_NAME}" || -z "${VERSION}" || -z "${ARCH}" || -z "${OUT_DIR}" ]]; then
  usage >&2
  exit 2
fi

if [[ -z "${DESC}" ]]; then
  DESC="${PACKAGE_NAME} command line tool"
fi

if [[ -z "${HOMEPAGE}" ]]; then
  HOMEPAGE="https://github.com/daulet/${PACKAGE_NAME}"
fi

if [[ ! -f "${BINARY_PATH}" ]]; then
  echo "binary not found: ${BINARY_PATH}" >&2
  exit 1
fi

case "${ARCH}" in
  amd64 | arm64)
    ;;
  *)
    echo "unsupported Debian architecture: ${ARCH}" >&2
    exit 1
    ;;
esac

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "${WORK_DIR}"' EXIT

PKG_ROOT="${WORK_DIR}/${PACKAGE_NAME}_${VERSION}_${ARCH}"
mkdir -p "${PKG_ROOT}/DEBIAN" "${PKG_ROOT}/usr/bin" "${PKG_ROOT}/usr/share/doc/${PACKAGE_NAME}"

install -m 755 "${BINARY_PATH}" "${PKG_ROOT}/usr/bin/${PACKAGE_NAME}"
if [[ -f README.md ]]; then
  install -m 644 README.md "${PKG_ROOT}/usr/share/doc/${PACKAGE_NAME}/README.md"
fi

INSTALLED_SIZE="$(du -sk "${PKG_ROOT}/usr" | awk '{print $1}')"

{
  cat <<EOF
Package: ${PACKAGE_NAME}
Version: ${VERSION}
Section: utils
Priority: optional
Architecture: ${ARCH}
Maintainer: ${MAINTAINER}
Installed-Size: ${INSTALLED_SIZE}
EOF
  if [[ -n "${DEPENDS}" ]]; then
    echo "Depends: ${DEPENDS}"
  fi
  cat <<EOF
Homepage: ${HOMEPAGE}
Description: ${DESC}
EOF
} > "${PKG_ROOT}/DEBIAN/control"

mkdir -p "${OUT_DIR}"
dpkg-deb --build --root-owner-group "${PKG_ROOT}" "${OUT_DIR}/${PACKAGE_NAME}_${VERSION}_${ARCH}.deb"
