#!/bin/sh
# THUGSred-smsGateway installer. Safe to run repeatedly: existing
# configuration, databases, logs, recordings and backups are never touched.
#
# Environment:
#   PREFIX        installation directory        (default /opt/THUGSred-smsGateway)
#   CONFDIR       configuration directory       (default /etc/thugsred)
#   DESTDIR       staging root for packaging    (default empty = live system)
#   BINARY        binary to install             (default target/release/thugsred-smsgateway)
#   SERVICE_USER  service account               (default thugsred-smsgateway)
set -eu

PREFIX="${PREFIX:-/opt/THUGSred-smsGateway}"
CONFDIR="${CONFDIR:-/etc/thugsred}"
DESTDIR="${DESTDIR:-}"
BINARY="${BINARY:-target/release/thugsred-smsgateway}"
SERVICE_USER="${SERVICE_USER:-thugsred-smsgateway}"
UNIT=thugsred-smsgateway.service
SRC="$(cd "$(dirname "$0")/.." && pwd)"
cd "$SRC"

say() { printf '  %s\n' "$*"; }
die() { printf 'error: %s\n' "$*" >&2; exit 1; }

case "$PREFIX" in /*) ;; *) die "PREFIX must be an absolute path" ;; esac
case "$CONFDIR" in /*) ;; *) die "CONFDIR must be an absolute path" ;; esac
[ -f "$BINARY" ] || die "$BINARY not found; run 'make release' first (as a normal user)"
if [ -z "$DESTDIR" ] && [ "$(id -u)" -ne 0 ]; then
    die "installation needs root; run: sudo make install"
fi

echo "Installing THUGSred-smsGateway into ${DESTDIR}${PREFIX}"

if [ -z "$DESTDIR" ]; then
    if ! getent group "$SERVICE_USER" >/dev/null; then
        groupadd --system "$SERVICE_USER"
        say "created group $SERVICE_USER"
    fi
    if ! getent passwd "$SERVICE_USER" >/dev/null; then
        useradd --system --gid "$SERVICE_USER" --home-dir /var/lib/thugsred-smsgateway \
            --no-create-home --shell /usr/sbin/nologin \
            --comment "THUGSred SMS Gateway" "$SERVICE_USER"
        say "created user $SERVICE_USER"
    fi
    for g in dialout audio; do
        if getent group "$g" >/dev/null; then
            usermod -aG "$g" "$SERVICE_USER"
        fi
    done
    install_owned() { install -d -m "$1" -o "$SERVICE_USER" -g "$SERVICE_USER" "$2"; }
    CONF_GROUP="$SERVICE_USER"
else
    install_owned() { install -d -m "$1" "$2"; }
    CONF_GROUP=""
fi

# Program files (always replaced).
install -d -m 0755 "$DESTDIR$PREFIX" "$DESTDIR$PREFIX/bin" "$DESTDIR$PREFIX/config" \
    "$DESTDIR$PREFIX/config/modem-profiles" "$DESTDIR$PREFIX/docs" "$DESTDIR$PREFIX/scripts" \
    "$DESTDIR$PREFIX/systemd"
# Install through a temporary name so a running daemon keeps its old binary.
install -m 0755 "$BINARY" "$DESTDIR$PREFIX/bin/.thugsred-smsgateway.new"
mv -f "$DESTDIR$PREFIX/bin/.thugsred-smsgateway.new" "$DESTDIR$PREFIX/bin/thugsred-smsgateway"
install -m 0644 config/*.example "$DESTDIR$PREFIX/config/"
install -m 0644 config/modem-profiles/*.toml "$DESTDIR$PREFIX/config/modem-profiles/"
for f in docs/*.md README.md LICENSE CHANGELOG.md SECURITY.md; do
    [ -f "$f" ] || continue
    case "$f" in
        docs/*) install -m 0644 "$f" "$DESTDIR$PREFIX/docs/" ;;
        *) install -m 0644 "$f" "$DESTDIR$PREFIX/" ;;
    esac
done
install -m 0755 scripts/*.sh "$DESTDIR$PREFIX/scripts/"
install -m 0644 scripts/*.rules scripts/*.example "$DESTDIR$PREFIX/scripts/"
sed "s|/opt/THUGSred-smsGateway|$PREFIX|g; s|/etc/thugsred|$CONFDIR|g" \
    "systemd/$UNIT" > "$DESTDIR$PREFIX/systemd/$UNIT"
chmod 0644 "$DESTDIR$PREFIX/systemd/$UNIT"
say "program files installed"

# Data directories (created when missing, never emptied).
install_owned 0750 "$DESTDIR$PREFIX/recordings"
install_owned 0750 "$DESTDIR$PREFIX/backups"
install_owned 0750 "$DESTDIR/var/lib/thugsred-smsgateway"
install_owned 0750 "$DESTDIR/var/lib/thugsred-smsgateway/state"
install_owned 0750 "$DESTDIR/var/lib/thugsred-smsgateway/spool"
install_owned 0750 "$DESTDIR/var/lib/thugsred-smsgateway/attachments"
install_owned 0750 "$DESTDIR/var/log/thugsred-smsgateway"

# Configuration: only files that do not exist yet are created.
install -d -m 0750 "$DESTDIR$CONFDIR" "$DESTDIR$CONFDIR/modem-profiles"
new_conf() { # <example> <target> <mode>
    if [ -e "$DESTDIR$CONFDIR/$2" ]; then
        say "kept existing $CONFDIR/$2"
    else
        (umask 077; sed "s|/opt/THUGSred-smsGateway|$PREFIX|g; s|/etc/thugsred|$CONFDIR|g" "config/$1" > "$DESTDIR$CONFDIR/$2")
        chmod "$3" "$DESTDIR$CONFDIR/$2"
        if [ -n "$CONF_GROUP" ]; then
            # Root owns the configuration; the service may read but not change it.
            if [ "$3" = "0600" ]; then chown root:root "$CONFDIR/$2"; else chown root:"$CONF_GROUP" "$CONFDIR/$2"; fi
        fi
        say "created $CONFDIR/$2"
    fi
}
new_conf smsgateway.conf.example smsgateway.conf 0640
new_conf sim.conf.example sim.conf 0640
new_conf integrations.conf.example integrations.conf 0640
new_conf addressbook.conf.example addressbook.conf 0640
new_conf anonymous.conf.example anonymous.conf 0640
new_conf smsgateway.env.example smsgateway.env 0600
if [ -n "$CONF_GROUP" ]; then
    chown root:"$CONF_GROUP" "$CONFDIR" "$CONFDIR/modem-profiles"
fi

if [ -z "$DESTDIR" ]; then
    install -m 0644 "$PREFIX/systemd/$UNIT" "/etc/systemd/system/$UNIT"
    if command -v systemctl >/dev/null 2>&1 && [ -d /run/systemd/system ]; then
        systemctl daemon-reload
    fi
    say "installed /etc/systemd/system/$UNIT"
    cat <<MSG

Installation complete. Nothing was enabled or started.

  1. Review   $CONFDIR/smsgateway.conf and $CONFDIR/sim.conf
  2. Check    $PREFIX/bin/thugsred-smsgateway check-config
  3. Start    sudo systemctl enable --now thugsred-smsgateway
  4. Verify   $PREFIX/bin/thugsred-smsgateway doctor

GSM modules such as the SIM868 must be powered on by hand (hold PWRKEY)
after they are attached; the gateway connects as soon as the module answers.
MSG
else
    install -d -m 0755 "$DESTDIR/lib/systemd/system"
    install -m 0644 "$DESTDIR$PREFIX/systemd/$UNIT" "$DESTDIR/lib/systemd/system/$UNIT"
    say "staged into $DESTDIR"
fi
