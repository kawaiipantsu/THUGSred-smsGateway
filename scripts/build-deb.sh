#!/bin/sh
# Build a Debian package from a release binary.
# Usage: scripts/build-deb.sh <binary> <deb-architecture> <version>
set -eu
cd "$(dirname "$0")/.."
[ $# -eq 3 ] || { echo "usage: $0 <binary> <deb-architecture> <version>" >&2; exit 1; }
BINARY="$1"; ARCH="$2"; VERSION="$3"
command -v dpkg-deb >/dev/null 2>&1 || { echo "error: dpkg-deb is required" >&2; exit 1; }

# The staging tree lives in a fresh private directory below dist/.
mkdir -p dist
STAGE="$(mktemp -d "dist/.deb-stage.XXXXXX")"
chmod 0755 "$STAGE"
cleanup() {
    case "$STAGE" in
        dist/.deb-stage.*) rm -rf -- "${STAGE:?}" ;;
    esac
}
trap cleanup EXIT

DESTDIR="$(pwd)/$STAGE" BINARY="$BINARY" sh scripts/install.sh >/dev/null
# Configuration is created by the maintainer script, never shipped as files,
# so that upgrades cannot overwrite a deployment's settings.
find "$STAGE/etc" -depth -delete

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<CTRL
Package: thugsred-smsgateway
Version: $VERSION
Section: comm
Priority: optional
Architecture: $ARCH
Depends: adduser, libc6
Suggests: alsa-utils
Maintainer: THUGSred <https://thugs.red>
Homepage: https://github.com/kawaiipantsu/THUGSred-smsGateway
Description: Self-hosted SMS gateway for GSM modems
 Rust SMS gateway for Linux and Raspberry Pi with GSM/PDU messaging, REST
 API, web dashboard, persistent message storage and service integrations.
CTRL
cat > "$STAGE/DEBIAN/postinst" <<'POST'
#!/bin/sh
set -e
U=thugsred-smsgateway
getent group "$U" >/dev/null || addgroup --system "$U"
getent passwd "$U" >/dev/null || adduser --system --ingroup "$U" --home /var/lib/thugsred-smsgateway --no-create-home --shell /usr/sbin/nologin "$U"
for g in dialout audio; do
    if getent group "$g" >/dev/null; then adduser "$U" "$g" >/dev/null || true; fi
done
install -d -m 0750 -o root -g "$U" /etc/thugsred /etc/thugsred/modem-profiles
for f in smsgateway sim integrations addressbook anonymous; do
    [ -e "/etc/thugsred/$f.conf" ] || install -m 0640 -o root -g "$U" "/opt/THUGSred-smsGateway/config/$f.conf.example" "/etc/thugsred/$f.conf"
done
[ -e /etc/thugsred/smsgateway.env ] || install -m 0600 /opt/THUGSred-smsGateway/config/smsgateway.env.example /etc/thugsred/smsgateway.env
for d in /var/lib/thugsred-smsgateway /var/log/thugsred-smsgateway /opt/THUGSred-smsGateway/recordings /opt/THUGSred-smsGateway/backups; do
    install -d -m 0750 -o "$U" -g "$U" "$d"
done
if [ -d /run/systemd/system ]; then systemctl daemon-reload || true; fi
echo "thugsred-smsgateway installed. Review /etc/thugsred/*.conf, then: systemctl enable --now thugsred-smsgateway"
POST
cat > "$STAGE/DEBIAN/prerm" <<'PRE'
#!/bin/sh
set -e
if [ -d /run/systemd/system ]; then systemctl stop thugsred-smsgateway.service || true; fi
PRE
chmod 0755 "$STAGE/DEBIAN/postinst" "$STAGE/DEBIAN/prerm"
OUT="dist/thugsred-smsgateway_${VERSION}_${ARCH}.deb"
dpkg-deb --root-owner-group --build "$STAGE" "$OUT" >/dev/null
echo "wrote $OUT"
