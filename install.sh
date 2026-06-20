#!/bin/bash
set -euo pipefail

# komari-agent-rs install script — Linux(systemd) / macOS(launchd) / FreeBSD(rc.d)
# https://github.com/DeliciousBuding/komari-agent-rs

GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'; CYAN='\033[0;36m'; NC='\033[0m'
ok()  { echo -e "${GREEN}[OK]${NC} $1"; }
err() { echo -e "${RED}[ERROR]${NC} $1"; exit 1; }
warn(){ echo -e "${YELLOW}[WARN]${NC} $1"; }
info(){ echo "  $1"; }

BIN_NAME="komari-agent-rs"
BIN_PATH="/usr/local/bin/${BIN_NAME}"
CONFIG_DIR="/etc/komari-agent"
CONFIG_PATH="${CONFIG_DIR}/config.json"
SERVICE_NAME="komari-agent-rs"
REPO="DeliciousBuding/komari-agent-rs"
GH_PROXY=""; VERSION=""; TOKEN=""; ENDPOINT=""; TMP_DIR=""

cleanup() { [ -n "${TMP_DIR}" ] && [ -d "${TMP_DIR}" ] && rm -rf "${TMP_DIR}"; }
trap cleanup EXIT

usage() {
  cat <<'EOF'
Usage: install.sh [OPTIONS]
  --token TOKEN      Agent auth token (written to config.json)
  --endpoint URL     Server endpoint URL (written to config.json)
  --version VERSION  Install a specific release (default: latest)
  --ghproxy URL      GitHub proxy base URL
  --help             Show this help
EOF
  exit 0
}

# parse args
while [[ $# -gt 0 ]]; do
  case "$1" in
    --token)    TOKEN="$2"; shift 2 ;;
    --endpoint) ENDPOINT="$2"; shift 2 ;;
    --version)  VERSION="$2"; shift 2 ;;
    --ghproxy)  GH_PROXY="$2"; shift 2 ;;
    --help)     usage ;;
    *) err "Unknown option: $1" ;;
  esac
done

[ "${EUID:-0}" -ne 0 ] && err "Please run as root (sudo ./install.sh)"

# banner
echo -e "  komari-agent-rs installer  |  version: ${CYAN}${VERSION:-latest}${NC}"
[ -n "${GH_PROXY}" ] && info "proxy: ${GH_PROXY}"

# detect OS
os_type=$(uname -s)
case "${os_type}" in
  Linux)   os="linux"   ;;
  Darwin)  os="darwin"  ;;
  FreeBSD) os="freebsd" ;;
  *)       err "Unsupported OS: ${os_type}" ;;
esac

# detect arch
arch=$(uname -m)
case "${arch}" in
  x86_64)        arch="amd64" ;;
  aarch64|arm64) arch="arm64" ;;
  i386|i686)     [ "${os}" = "darwin" ] && err "32-bit x86 not supported on darwin"; arch="386" ;;
  armv7*|armv6*) [ "${os}" = "darwin" ] && err "32-bit ARM not supported on darwin"; arch="arm"  ;;
  *)             err "Unsupported architecture: ${arch}" ;;
esac
info "Detected: ${GREEN}${os}/${arch}${NC}"

# prereqs
for cmd in curl sha256sum; do
  if ! command -v "${cmd}" >/dev/null 2>&1; then
    [ "${cmd}" = "sha256sum" ] && [ "${os}" = "darwin" ] && command -v shasum >/dev/null 2>&1 && continue
    err "'${cmd}' is required but not found"
  fi
done

# download URLs
file="${BIN_NAME}-${os}-${arch}"
checksums="checksums.txt"
rel="latest/download"; [ -n "${VERSION}" ] && rel="download/${VERSION}"
base="https://github.com/${REPO}/releases/${rel}"
bin_url="${base}/${file}"; chk_url="${base}/${checksums}"
[ -n "${GH_PROXY}" ] && bin_url="${GH_PROXY}/${base}/${file}" && chk_url="${GH_PROXY}/${base}/${checksums}"

# temp dir
TMP_DIR=$(mktemp -d -t komari-agent-rs-install-XXXXXXXX)

# download binary
info "Downloading ${file} ..."
curl -fSL --progress-bar -o "${TMP_DIR}/${file}" "${bin_url}" || err "Download failed: ${bin_url}"
ok "Binary downloaded"

# sha256 verify
info "Verifying checksum ..."
if curl -fSL --progress-bar -o "${TMP_DIR}/${checksums}" "${chk_url}" 2>/dev/null; then
  expected=$(grep "${file}" "${TMP_DIR}/${checksums}" | awk '{print $1}' | head -1)
  if [ -n "${expected}" ]; then
    if command -v sha256sum >/dev/null 2>&1; then
      actual=$(sha256sum "${TMP_DIR}/${file}" | awk '{print $1}')
    else
      actual=$(shasum -a 256 "${TMP_DIR}/${file}" | awk '{print $1}')
    fi
    [ "${expected}" = "${actual}" ] || err "SHA256 mismatch!\n  Expected: ${expected}\n  Actual:   ${actual}"
    ok "SHA256 verified"
  else
    warn "Hash entry not found — skipping verification"
  fi
else
  warn "Checksums not available — skipping verification"
fi

# install binary
install -m 755 "${TMP_DIR}/${file}" "${BIN_PATH}"
ok "Installed to ${BIN_PATH}"

# config template
mkdir -p "${CONFIG_DIR}"
if [ -f "${CONFIG_PATH}" ]; then
  warn "Config already exists — not overwriting"
else
  cat > "${CONFIG_PATH}" <<EOF
{
  "token": "${TOKEN}",
  "endpoint": "${ENDPOINT}",
  "log_level": "info"
}
EOF
  chmod 600 "${CONFIG_PATH}"
  ok "Config: ${CONFIG_PATH}"
  [ -n "${TOKEN}" ] && info "  token: (set)"
  [ -n "${ENDPOINT}" ] && info "  endpoint: ${ENDPOINT}"
fi

# ── register service ──────────────────────────────────────────────────────

info "Setting up service (${os}) ..."

# systemd (Linux)
if command -v systemctl >/dev/null 2>&1 && systemctl list-units >/dev/null 2>&1; then
  SERVICE_FILE="/etc/systemd/system/${SERVICE_NAME}.service"
  cat > "${SERVICE_FILE}" <<EOF
[Unit]
Description=komari-agent-rs monitoring agent
After=network.target
Documentation=https://github.com/${REPO}

[Service]
Type=simple
ExecStart=${BIN_PATH} --config ${CONFIG_PATH}
Restart=always
RestartSec=10
User=root
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
EOF
  systemctl daemon-reload
  systemctl enable "${SERVICE_NAME}.service"
  systemctl start "${SERVICE_NAME}.service" 2>/dev/null || warn "Start failed — check: journalctl -u ${SERVICE_NAME}"
  ok "systemd service registered and started"

# launchd (macOS)
elif [ "${os}" = "darwin" ] && command -v launchctl >/dev/null 2>&1; then
  PLIST="/Library/LaunchDaemons/com.komari.${SERVICE_NAME}.plist"
  cat > "${PLIST}" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.komari.${SERVICE_NAME}</string>
    <key>ProgramArguments</key>
    <array><string>${BIN_PATH}</string><string>--config</string><string>${CONFIG_PATH}</string></array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/var/log/${SERVICE_NAME}.log</string>
    <key>StandardErrorPath</key><string>/var/log/${SERVICE_NAME}.log</string>
</dict>
</plist>
EOF
  launchctl bootstrap system "${PLIST}" 2>/dev/null || warn "Bootstrap failed — try: launchctl load ${PLIST}"
  ok "launchd service registered"

# rc.d (FreeBSD)
elif [ "${os}" = "freebsd" ]; then
  RC_FILE="/usr/local/etc/rc.d/${SERVICE_NAME}"
  cat > "${RC_FILE}" <<'RCEOF'
#!/bin/sh
# PROVIDE: komari_agent_rs
# REQUIRE: DAEMON NETWORKING
# KEYWORD: shutdown

. /etc/rc.subr

name="komari_agent_rs"
rcvar="komari_agent_rs_enable"
command="/usr/local/bin/komari-agent-rs"
command_args="--config /etc/komari-agent/config.json"
pidfile="/var/run/${name}.pid"

load_rc_config "${name}"
: ${komari_agent_rs_enable:="NO"}

run_rc_command "$1"
RCEOF
  chmod 755 "${RC_FILE}"
  grep -q 'komari_agent_rs_enable' /etc/rc.conf 2>/dev/null || echo 'komari_agent_rs_enable="YES"' >> /etc/rc.conf
  ok "rc.d service installed at ${RC_FILE}"
  info "Run: service komari_agent_rs start"

else
  err "No supported init system detected (systemd / launchd / rc.d)"
fi

# done
echo ""
echo -e "  Binary : ${GREEN}${BIN_PATH}${NC}"
echo -e "  Config : ${GREEN}${CONFIG_PATH}${NC}"
echo -e "  Service: ${GREEN}${SERVICE_NAME}${NC}"
echo ""
case "${os}" in
  linux)   info "systemctl status/start/stop/restart ${SERVICE_NAME}" ;;
  darwin)  info "launchctl bootout/kickstart system ${PLIST:-/Library/LaunchDaemons/com.komari.${SERVICE_NAME}.plist}" ;;
  freebsd) info "service komari_agent_rs status/start/stop/restart" ;;
esac
ok "Installation complete"
