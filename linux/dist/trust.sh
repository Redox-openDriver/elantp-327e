root_process() {
  for trust_entry in /proc/[0-9]*; do
    { read -r trust_name < "$trust_entry/comm"; } 2>/dev/null || continue
    [ "$trust_name" = "$1" ] || continue
    [ "$(awk '/^Uid:/ { print $2 "-" $3 "-" $4 "-" $5 }' "$trust_entry/status" 2>/dev/null)" = 0-0-0-0 ] || continue
    trust_program=$(as_root readlink "$trust_entry/exe" 2>/dev/null) || continue
    case ${trust_program##*/} in
      "$1" | busybox) ;;
      *) continue ;;
    esac
    printf '%s\n' "${trust_entry#/proc/}"
    return 0
  done
  return 1
}

trusted_dir() {
  [ -n "$1" ] || return 1
  if [ -L "$1" ]; then
    [ "$(stat -c '%u' "$1" 2>/dev/null)" = 0 ] || return 1
    trusted_dir "$(dirname "$1")" || return 1
    set -- "$(readlink -f "$1")"
  fi
  [ -d "$1" ] && [ ! -L "$1" ] || return 1
  case $(stat -c '%u %a' "$1" 2>/dev/null) in
    '0 '*[2367]? | '0 '*[2367]) return 1 ;;
    '0 '*) return 0 ;;
    *) return 1 ;;
  esac
}

watched_by() {
  trust_supervisor=$(root_process "$1") || return 1
  trust_scan=$(tr '\0' '\n' < "/proc/$trust_supervisor/cmdline" 2>/dev/null | grep '^/' | tail -n 1) || return 1
  trusted_dir "$trust_scan" || return 1
  printf '%s\n' "$trust_scan"
}
