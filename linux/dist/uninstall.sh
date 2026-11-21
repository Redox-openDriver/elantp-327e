#!/bin/sh
set -eu

here=$(dirname "$0")
catalogs=$here/locales
. "$here/messages.sh"
language=$(language)
mode=${1:-}

say() { text "$@"; }
fail() { text "$@" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

case $mode in
  '' | --purge) ;;
  *) fail remove-usage ;;
esac

if [ "$(id -u)" -eq 0 ]; then
  as_root() { "$@"; }
elif have sudo; then
  as_root() { sudo "$@"; }
elif have doas; then
  as_root() { doas "$@"; }
else
  fail remove-root-needed
fi

. "$here/trust.sh"
watched=$(watched_by runsvdir || true)

name=elantpd
if [ -e "/etc/systemd/system/$name.service" ]; then
  as_root systemctl disable --now "$name.service" 2>/dev/null || true
  as_root rm -f "/etc/systemd/system/$name.service"
fi
if [ -e "/etc/init.d/$name" ]; then
  as_root rc-service "$name" stop 2>/dev/null || true
  as_root rc-update del "$name" default 2>/dev/null || true
  as_root rm -f "/etc/init.d/$name"
fi
for services in /etc/sv /etc/runit/sv; do
  if [ -e "$services/$name" ]; then
    for scan in "$watched" /var/service /run/runit/service /etc/service /etc/runit/runsvdir/default; do
      trusted_dir "$scan" && [ -L "$scan/$name" ] || continue
      as_root env SVDIR="$scan" sv down "$name" 2>/dev/null || true
      as_root rm -f "$scan/$name"
    done
    as_root rm -rf "$services/$name"
  fi
done
if [ -e "/etc/dinit.d/$name" ]; then
  as_root dinitctl stop "$name" 2>/dev/null || true
  as_root dinitctl disable "$name" 2>/dev/null || true
  as_root rm -f "/etc/dinit.d/$name"
fi
supervised=$(watched_by s6-svscan || true)
if trusted_dir "$supervised" && [ -L "$supervised/$name" ]; then
  as_root s6-svc -d "$supervised/$name" 2>/dev/null || true
  as_root rm -f "$supervised/$name"
  as_root s6-svscanctl -an "$supervised" 2>/dev/null || true
fi
as_root rm -rf /etc/elantp/s6
if [ -d /run/systemd/system ]; then
  as_root systemctl daemon-reload
fi
as_root rm -f /usr/local/bin/elantpd /usr/local/bin/elantp-dev /usr/local/bin/elantp-notify /etc/xdg/autostart/elantp-notify.desktop
as_root rm -rf /usr/local/share/doc/elantp

if [ "$mode" = --purge ]; then
  as_root rm -rf /etc/elantp
  say remove-purged
else
  say remove-kept dir=/etc/elantp
fi
