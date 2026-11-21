#!/bin/sh
set -eu
cd "$(dirname "$0")/../.."

dist=linux/dist
manager=${ELANTP_SERVICE_MANAGER:-}
documents=/usr/local/share/doc/elantp

catalogs=$dist/locales
. "$dist/messages.sh"
language=$(language)

say() { text "$@"; }
item() { printf '  %s\n' "$(text "$@")"; }
fail() { text "$@" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
. "$dist/trust.sh"
running() { root_process "$1" >/dev/null; }
trap 'status=$?; [ "$status" -eq 0 ] || text setup-stopped script=install.sh status="$status" >&2' EXIT

if [ "$(id -u)" -eq 0 ]; then
  [ -z "${SUDO_USER:-}${DOAS_USER:-}" ] || fail setup-not-with-sudo
  as_root() { "$@"; }
elif have sudo; then
  as_root() { sudo "$@"; }
elif have doas; then
  as_root() { doas "$@"; }
else
  fail setup-root-needed
fi

detect_manager() {
  init=$(cat /proc/1/comm 2>/dev/null || true)
  if [ -d /run/systemd/system ]; then
    echo systemd
  elif [ -e /run/openrc/softlevel ] || [ "$init" = openrc-init ]; then
    echo openrc
  elif [ "$init" = runit ] || running runsvdir; then
    echo runit
  elif [ "$init" = dinit ] || running dinit; then
    echo dinit
  elif [ "$init" = s6-svscan ] || running s6-svscan; then
    echo s6
  else
    echo none
  fi
}

start_service() {
  name=$1
  case $manager in
    systemd)
      as_root systemctl enable "$name.service" >/dev/null 2>&1
      as_root systemctl restart "$name.service"
      sleep 1
      as_root systemctl is-active --quiet "$name.service" || return 1
      ;;
    openrc)
      as_root install -Dm755 "$dist/openrc/$name" "/etc/init.d/$name"
      as_root rc-update add "$name" default >/dev/null
      as_root rc-service "$name" restart >/dev/null
      sleep 1
      as_root rc-service "$name" status >/dev/null 2>&1 || return 1
      ;;
    runit)
      if [ -z "$scan" ] || [ ! -d "$scan" ]; then
        return 2
      fi
      as_root ln -sfn "$services/$name" "$scan/$name"
      sleep 6
      as_root env SVDIR="$scan" sv restart "$name" >/dev/null 2>&1 || true
      sleep 1
      as_root env SVDIR="$scan" sv status "$name" 2>/dev/null | grep -q '^run:' || return 1
      ;;
    dinit)
      as_root dinitctl enable "$name" >/dev/null 2>&1 || true
      as_root dinitctl restart "$name" >/dev/null 2>&1 || as_root dinitctl start "$name" >/dev/null
      sleep 1
      as_root dinitctl status "$name" 2>/dev/null | grep -q 'STARTED' || return 1
      ;;
    *)
      return 2
      ;;
  esac
}

[ -n "$manager" ] || manager=$(detect_manager)
case $manager in
  systemd | openrc | runit | dinit | s6 | none) ;;
  *) fail setup-bad-manager ;;
esac
have cargo || fail setup-no-cargo

cargo build --release --locked -p elantp-linux -p elantp-notify
built=$(cargo metadata --no-deps --format-version 1 | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')/release
for artifact in elantpd elantp-dev elantp-notify; do
  [ -f "$built/$artifact" ] || fail setup-no-artifact path="$built/$artifact"
done

as_root install -Dm755 "$built/elantpd" /usr/local/bin/elantpd
as_root install -Dm755 "$built/elantp-dev" /usr/local/bin/elantp-dev
as_root install -Dm755 "$built/elantp-notify" /usr/local/bin/elantp-notify
as_root install -Dm644 "$dist/notify/elantp-notify.desktop" /etc/xdg/autostart/elantp-notify.desktop
as_root install -Dm644 README.md "$documents/README.md"
as_root install -Dm644 docs/gestures.md "$documents/gestures.md"
as_root install -Dm644 config/gestures.toml "$documents/gestures.toml"
library=$(cargo metadata --format-version 1 --locked | tr ',' '\n' | sed -n 's|^"manifest_path":"\(.*/gestus[^/]*/[^/]*/Cargo.toml\)"$|\1|p' | head -n 1)
if [ -n "$library" ] && [ -f "${library%/Cargo.toml}/docs/settings.md" ]; then
  as_root install -Dm644 "${library%/Cargo.toml}/docs/settings.md" "$documents/settings.md"
fi
as_root install -d -m 0755 /etc/elantp

services=/etc/sv
scan=
case $manager in
  systemd)
    as_root install -Dm644 "$dist/systemd/elantpd.service" /etc/systemd/system/elantpd.service
    as_root systemctl daemon-reload
    ;;
  runit)
    [ -d "$services" ] || [ ! -d /etc/runit/sv ] || services=/etc/runit/sv
    scan=$(watched_by runsvdir || true)
    as_root install -Dm755 "$dist/runit/run" "$services/elantpd/run"
    ;;
  dinit)
    as_root install -Dm644 "$dist/dinit/elantpd" /etc/dinit.d/elantpd
    ;;
  s6)
    as_root install -Dm755 "$dist/s6/run" /etc/elantp/s6/elantpd/run
    as_root install -Dm644 "$dist/s6/type" /etc/elantp/s6/elantpd/type
    ;;
esac

started=yes
start_service elantpd || { [ $? -eq 2 ] && started=manual || started=no; }
[ "$started" != no ] || fail setup-service-failed manager="$manager"

echo
say setup-manager manager="$manager"
case $started-$manager in
  manual-s6) say setup-service-s6 path=/etc/elantp/s6/elantpd ;;
  manual-runit) say setup-service-runit path="$services/elantpd" ;;
  manual-*) say setup-service-none program=/usr/local/bin/elantpd ;;
  *) say setup-service-running ;;
esac

elevate=sudo
have sudo || ! have doas || elevate=doas
echo
say setup-next
item setup-next-desktop
item setup-next-settings example="$documents/gestures.toml" file=/etc/elantp/gestures.toml
item setup-next-guide guide="$documents/gestures.md"
item setup-next-learn elevate="$elevate"
item setup-next-notify program=/usr/local/bin/elantp-notify
