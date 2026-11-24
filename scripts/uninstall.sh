#!/bin/sh
# THUGSred-smsGateway uninstaller.
#
# Default: removes the service and program files. Configuration, message
# database, logs, recordings and backups are KEPT.
#
#   uninstall.sh            remove program files only
#   uninstall.sh --purge    additionally delete ALL data and configuration
#                           (asks for confirmation; cannot be undone)
set -eu

PREFIX="${PREFIX:-/opt/THUGSred-smsGateway}"
CONFDIR="${CONFDIR:-/etc/thugsred}"
SERVICE_USER="${SERVICE_USER:-thugsred-smsgateway}"
UNIT=thugsred-smsgateway.service
DATADIR=/var/lib/thugsred-smsgateway
LOGDIR=/var/log/thugsred-smsgateway
RUNDIR=/run/thugsred-smsgateway
PURGE=0
[ "${1:-}" = "--purge" ] && PURGE=1

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

[ "$(id -u)" -eq 0 ] || die "run as root (sudo make uninstall)"

# Refuse locations that could never be a dedicated installation directory.
# Everything below is removed relative to these two paths.
safe_dir() { # <name> <value>
    case "$2" in
        /*) ;;
        *) die "$1 must be an absolute path (got '$2')" ;;
    esac
    case "$2" in
        /|/bin|/boot|/dev|/etc|/home|/lib|/lib64|/opt|/proc|/root|/run|/sbin|/srv|/sys|/tmp|/usr|/usr/*|/var|/var/lib|/var/log)
            die "$1='$2' is a system directory; refusing to uninstall from it" ;;
    esac
    case "$2" in
        *..*|*//*) die "$1='$2' is not a clean path" ;;
    esac
}
safe_dir PREFIX "$PREFIX"
safe_dir CONFDIR "$CONFDIR"
# Only a directory that this project installed into is touched.
if [ -d "$PREFIX" ] && [ ! -e "$PREFIX/bin/thugsred-smsgateway" ] && [ ! -d "$PREFIX/recordings" ]; then
    die "$PREFIX does not look like a THUGSred-smsGateway installation; nothing removed"
fi

if [ "$PURGE" -eq 1 ]; then
    cat <<MSG
PURGE will permanently delete:
  $CONFDIR                           (configuration, SIM settings, secrets)
  $DATADIR       (message database and attachments)
  $LOGDIR       (logs)
  $PREFIX                            (including recordings and backups)
  the service account $SERVICE_USER
MSG
    if [ "${PURGE_CONFIRM:-}" != "delete-all-gateway-data" ]; then
        printf 'Type "delete-all-gateway-data" to continue: '
        read -r answer
        [ "$answer" = "delete-all-gateway-data" ] || { echo "aborted; nothing was deleted"; exit 1; }
    fi
fi

if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    systemctl disable --now "$UNIT" >/dev/null 2>&1 || true
fi
rm -f "/etc/systemd/system/$UNIT"
if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
    systemctl daemon-reload
fi

# Program files only. `${PREFIX:?}` aborts instead of expanding to nothing.
for sub in bin config docs scripts systemd; do
    rm -rf "${PREFIX:?}/${sub:?}"
done
for file in README.md LICENSE CHANGELOG.md SECURITY.md; do
    rm -f "${PREFIX:?}/${file:?}"
done
echo "removed the service and program files"

if [ "$PURGE" -eq 1 ]; then
    rm -rf "${CONFDIR:?}"
    rm -rf "${DATADIR:?}" "${LOGDIR:?}" "${RUNDIR:?}"
    rm -rf "${PREFIX:?}"
    if getent passwd "$SERVICE_USER" >/dev/null; then userdel "$SERVICE_USER" || true; fi
    if getent group "$SERVICE_USER" >/dev/null; then groupdel "$SERVICE_USER" || true; fi
    echo "purged all gateway data and configuration"
else
    cat <<MSG
kept (remove by hand or with --purge if no longer needed):
  $CONFDIR
  $DATADIR
  $LOGDIR
  $PREFIX/recordings  $PREFIX/backups
MSG
fi
