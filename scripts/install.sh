#!/usr/bin/env bash
set -euo pipefail

PASS_STEPS=()
FAIL_STEPS=()

step_start() {
  printf '[..] %s\n' "$1"
}

step_ok() {
  PASS_STEPS+=("$1")
  printf '[OK] %s\n' "$1"
}

step_fail() {
  FAIL_STEPS+=("$1")
  printf '[FAIL] %s: %s\n' "$1" "$2" >&2
}

run_step() {
  local name="$1"
  shift

  step_start "$name"
  if "$@"; then
    step_ok "$name"
  else
    step_fail "$name" "command failed"
    return 1
  fi
}

require_root() {
  [[ "${EUID}" -eq 0 ]]
}

check_os() {
  source /etc/os-release
  [[ "${ID:-}" == "ubuntu" && "${VERSION_ID:-}" == "24.04" ]]
}

check_tools() {
  local tool
  for tool in apt-get dkms modprobe udevadm systemctl useradd usermod getent stat install; do
    command -v "$tool" >/dev/null 2>&1 || return 1
  done
}

install_packages() {
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    dkms \
    build-essential \
    gstreamer1.0-pipewire \
    gstreamer1.0-tools \
    libdrm-dev \
    "linux-headers-$(uname -r)" \
    evdi-dkms \
    libevdi-dev \
    xdg-desktop-portal
  dpkg -s evdi-dkms libevdi-dev gstreamer1.0-pipewire gstreamer1.0-tools xdg-desktop-portal >/dev/null 2>&1
}

create_system_user() {
  if id desplio >/dev/null 2>&1; then
    return 0
  fi

  useradd \
    --system \
    --create-home \
    --home-dir /var/lib/desplio \
    --shell /usr/sbin/nologin \
    desplio
}

assign_groups() {
  local group
  for group in video render input; do
    getent group "${group}" >/dev/null 2>&1 || return 1
    usermod -aG "${group}" desplio
  done
}

install_udev_rule() {
  install -Dm0644 /dev/null /etc/udev/rules.d/80-desplio-uinput.rules
  printf '%s\n' 'KERNEL=="uinput", MODE="0660", GROUP="input", OPTIONS+="static_node=uinput"' \
    >/etc/udev/rules.d/80-desplio-uinput.rules
  udevadm control --reload

  if ! [[ -e /dev/uinput ]]; then
    modprobe uinput || true
  fi

  udevadm trigger --verbose --action=add --subsystem-match=misc --sysname-match=uinput || true
  udevadm settle || true

  if [[ -e /dev/uinput ]]; then
    chgrp input /dev/uinput || true
    chmod 0660 /dev/uinput || true
  fi

  local state
  state="$(stat -c '%a %G' /dev/uinput)"
  [[ "${state}" == "660 input" ]]
}

install_modprobe_config() {
  install -Dm0644 /dev/null /etc/modprobe.d/desplio-evdi.conf
  printf 'options evdi initial_device_count=1\n' >/etc/modprobe.d/desplio-evdi.conf
}

install_modules_load_config() {
  install -Dm0644 /dev/null /etc/modules-load.d/desplio.conf
  printf 'evdi\nuinput\n' >/etc/modules-load.d/desplio.conf
}

load_modules_now() {
  modprobe -r evdi 2>/dev/null || true
  modprobe evdi initial_device_count=1
  modprobe uinput || true
  [[ -d /sys/module/evdi && -d /sys/module/uinput ]] || [[ -d /sys/module/evdi ]]
  [[ "$(cat /sys/module/evdi/parameters/initial_device_count 2>/dev/null)" == "1" ]]
}

print_summary() {
  printf '\nSummary\n'
  printf 'Passed: %s\n' "${#PASS_STEPS[@]}"
  printf 'Failed: %s\n' "${#FAIL_STEPS[@]}"
  printf '\nFinal verification\n'
  dkms status evdi || true
  lsmod | grep '^evdi' || true
  ls -l /dev/uinput || true
  id desplio || true
  printf '\nNext steps\n'
  printf '%s\n' 'X11 support now uses session-level runtime output activation only; no boot-time Xorg dummy config is installed.'
  printf '%s\n' 'Wayland virtual-monitor capture currently uses the PipeWire portal plus the GStreamer pipewiresrc bridge; the installer now ensures those host packages are present.'
  printf '%s\n' 'If a previous /etc/X11/xorg.conf.d/90-desplio-dummy.conf exists from older experiments, remove it before rebooting.'
}

main() {
  run_step "Preflight: root access" require_root || exit 1
  run_step "Preflight: Ubuntu 24.04" check_os || exit 1
  run_step "Preflight: required tools" check_tools || exit 1
  run_step "Install packages" install_packages || { print_summary; exit 1; }
  run_step "Create desplio system user" create_system_user || { print_summary; exit 1; }
  run_step "Assign desplio group memberships" assign_groups || { print_summary; exit 1; }
  run_step "Install uinput udev rule" install_udev_rule || { print_summary; exit 1; }
  run_step "Install evdi modprobe config" install_modprobe_config || { print_summary; exit 1; }
  run_step "Install boot-time module config" install_modules_load_config || { print_summary; exit 1; }
  run_step "Load evdi and uinput modules" load_modules_now || { print_summary; exit 1; }
  print_summary
}

main "$@"
