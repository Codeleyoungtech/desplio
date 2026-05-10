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
  for tool in apt-get dkms modprobe udevadm systemctl useradd usermod getent stat; do
    command -v "$tool" >/dev/null 2>&1 || return 1
  done
}

install_packages() {
  apt-get update
  DEBIAN_FRONTEND=noninteractive apt-get install -y \
    dkms \
    build-essential \
    libdrm-dev \
    "linux-headers-$(uname -r)" \
    evdi-dkms \
    libevdi-dev
  dkms status evdi | grep -q 'evdi/'
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
  modprobe uinput
  udevadm trigger /dev/uinput || true

  local state
  state="$(stat -c '%a %G' /dev/uinput)"
  [[ "${state}" == "660 input" ]]
}

install_modules_load_config() {
  install -Dm0644 /dev/null /etc/modules-load.d/desplio.conf
  printf 'evdi\nuinput\n' >/etc/modules-load.d/desplio.conf
}

load_modules_now() {
  modprobe evdi
  modprobe uinput
  [[ -d /sys/module/evdi && -d /sys/module/uinput ]]
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
}

main() {
  run_step "Preflight: root access" require_root || exit 1
  run_step "Preflight: Ubuntu 24.04" check_os || exit 1
  run_step "Preflight: required tools" check_tools || exit 1
  run_step "Install packages" install_packages || { print_summary; exit 1; }
  run_step "Create desplio system user" create_system_user || { print_summary; exit 1; }
  run_step "Assign desplio group memberships" assign_groups || { print_summary; exit 1; }
  run_step "Install uinput udev rule" install_udev_rule || { print_summary; exit 1; }
  run_step "Install boot-time module config" install_modules_load_config || { print_summary; exit 1; }
  run_step "Load evdi and uinput modules" load_modules_now || { print_summary; exit 1; }
  print_summary
}

main "$@"
