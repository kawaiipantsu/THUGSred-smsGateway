# Installation

This document covers building from source, what the installer does, uninstalling, and the
packaging targets. For Raspberry Pi specifics see [RASPBERRY-PI.md](RASPBERRY-PI.md).

- [Prerequisites](#prerequisites)
- [Build](#build)
- [Install](#install)
- [What the installer creates](#what-the-installer-creates)
- [PREFIX, CONFDIR and DESTDIR](#prefix-confdir-and-destdir)
- [Service account and groups](#service-account-and-groups)
- [First start](#first-start)
- [Running the command-line tool](#running-the-command-line-tool)
- [The systemd unit](#the-systemd-unit)
- [Uninstall and purge](#uninstall-and-purge)
- [Debian package](#debian-package)
- [Release archive](#release-archive)
- [Cross-compilation](#cross-compilation)

## Prerequisites

| Requirement | Notes |
| --- | --- |
| Linux with systemd | Developed and tested on Debian 13, x86_64 |
| Rust toolchain | `cargo` and `rustc`; the crate declares `rust-version = "1.82"`. Install from <https://rustup.rs> or with `apt install cargo`. |
| C compiler and linker | `sudo apt install build-essential` |
| `make` | Part of `build-essential` |
| Optional | `cargo-audit` and `cargo-deny` (`cargo install --locked cargo-audit cargo-deny`), `gitleaks`, `alsa-utils` (call recording), `python3` (SBOM), `dpkg-deb` (Debian package) |

`make deps` checks for `cargo` and a C compiler, prints the toolchain versions and downloads
the locked dependencies.

No system libraries are needed for TLS or the databases: TLS uses rustls, and SQLite is
compiled into the binary.

## Build

Build as a normal user. The Makefile prints a warning when a build target is run as root (set
`ALLOW_ROOT_BUILD=1` to silence it).

```sh
make deps
make release
```

The result is `target/release/thugsred-smsgateway`. Release builds remap source paths so that
local user names and directories do not end up in the binary. The embedded build metadata is
limited to the commit id, the build date (or `SOURCE_DATE_EPOCH`) and the target triple.

`make build` produces a debug build in `target/debug/`.

## Install

```sh
sudo make install
```

`make install` refuses to run when `target/release/thugsred-smsgateway` does not exist, and
the installer refuses to run without root on a live system. The installer is
[scripts/install.sh](../scripts/install.sh); it is safe to run repeatedly.

It finishes with the message "Installation complete. Nothing was enabled or started." and the
next steps.

## What the installer creates

Program files — **always replaced**:

| Path | Mode | Contents |
| --- | --- | --- |
| `/opt/THUGSred-smsGateway/bin/thugsred-smsgateway` | `0755` | The binary. Installed under a temporary name and renamed, so a running daemon keeps its old binary until it is restarted. |
| `/opt/THUGSred-smsGateway/config/*.example` | `0644` | Example configuration files |
| `/opt/THUGSred-smsGateway/config/modem-profiles/*.toml` | `0644` | Copies of the built-in modem profiles, for reference |
| `/opt/THUGSred-smsGateway/docs/`, `README.md`, `LICENSE`, `CHANGELOG.md`, `SECURITY.md` | `0644` | Documentation |
| `/opt/THUGSred-smsGateway/scripts/` | `0755` (`*.sh`), `0644` (`*.rules`, `*.example`) | Installer scripts, udev rule, nginx example |
| `/opt/THUGSred-smsGateway/systemd/thugsred-smsgateway.service` | `0644` | Unit file with `PREFIX` and `CONFDIR` substituted |
| `/etc/systemd/system/thugsred-smsgateway.service` | `0644` | Installed unit; `systemctl daemon-reload` is run |

Data directories — **created when missing, never emptied**, owned by the service account,
mode `0750`:

| Path | Purpose |
| --- | --- |
| `/opt/THUGSred-smsGateway/recordings` | Call recordings |
| `/opt/THUGSred-smsGateway/backups` | Database backups |
| `/var/lib/thugsred-smsgateway` | Message database (`messages.db`) |
| `/var/lib/thugsred-smsgateway/attachments` | Downloaded MMS content |
| `/var/lib/thugsred-smsgateway/state`, `/var/lib/thugsred-smsgateway/spool` | Created by the installer; not used by this release |
| `/var/log/thugsred-smsgateway` | Log files |

Configuration — **only files that do not exist yet are created**; existing files are reported
as "kept existing" and left untouched:

| Path | Owner | Mode |
| --- | --- | --- |
| `/etc/thugsred/` and `/etc/thugsred/modem-profiles/` | `root:thugsred-smsgateway` | `0750` |
| `/etc/thugsred/smsgateway.conf` | `root:thugsred-smsgateway` | `0640` |
| `/etc/thugsred/sim.conf` | `root:thugsred-smsgateway` | `0640` |
| `/etc/thugsred/integrations.conf` | `root:thugsred-smsgateway` | `0640` |
| `/etc/thugsred/addressbook.conf` | `root:thugsred-smsgateway` | `0640` |
| `/etc/thugsred/anonymous.conf` | `root:thugsred-smsgateway` | `0640` |
| `/etc/thugsred/smsgateway.env` | `root:root` | `0600` |

Root owns the configuration: the service can read it but cannot change it. The environment
file is read by systemd (as root) on behalf of the service, and directly by the command-line
tool when that runs as root; it is not readable by the service account or other users.

The installer **never**:

- overwrites existing configuration, databases, logs, recordings or backups;
- enables or starts the service;
- installs the udev rule, changes boot settings, or touches ModemManager.

`make config` creates missing configuration files from the examples on its own, also without
overwriting anything.

## PREFIX, CONFDIR and DESTDIR

| Variable | Default | Effect |
| --- | --- | --- |
| `PREFIX` | `/opt/THUGSred-smsGateway` | Installation directory. Must be absolute. |
| `CONFDIR` | `/etc/thugsred` | Configuration directory. Must be absolute. |
| `DESTDIR` | empty | Staging root for packaging. When set, no account is created, nothing is chowned, the unit goes to `$DESTDIR/lib/systemd/system/`, and root is not required. |
| `BINARY` | `target/release/thugsred-smsgateway` | Binary to install (environment variable of the script) |
| `SERVICE_USER` | `thugsred-smsgateway` | Service account (environment variable of the script) |

```sh
sudo make install PREFIX=/opt/smsgw CONFDIR=/etc/smsgw
```

With a non-default `PREFIX` or `CONFDIR` the installer substitutes the paths in the unit file
and in newly created configuration files. Two things remain your responsibility:

- The directories under `/var/lib` and `/var/log` are fixed; they do not follow `PREFIX`.
- The command-line tool's default configuration path is `/etc/thugsred/smsgateway.conf`. With
  a different `CONFDIR` pass `--config`, or set `THUGSRED_SMSGW_CONFIG`.

## Service account and groups

On a live installation the installer:

1. creates the system group `thugsred-smsgateway` if missing;
2. creates the system user `thugsred-smsgateway` if missing, with home
   `/var/lib/thugsred-smsgateway` (not created by `useradd`), shell `/usr/sbin/nologin`;
3. adds the user to the groups `dialout` and `audio` when those groups exist.

`dialout` gives access to serial devices; `audio` is only needed for call recording through
ALSA. The unit file lists both as `SupplementaryGroups`. If your serial device belongs to a
different group, add the service user to it:

```sh
ls -l /dev/ttyUSB0
sudo usermod -aG <group> thugsred-smsgateway
sudo systemctl restart thugsred-smsgateway
```

## First start

```sh
sudoedit /etc/thugsred/smsgateway.conf       # at least [modem] device
sudoedit /etc/thugsred/sim.conf
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway check-config
sudo systemctl enable --now thugsred-smsgateway
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway doctor
```

If the module needs it, power it on with PWRKEY after attaching it; see
[MODEMS.md](MODEMS.md#power-on-and-the-pwrkey-hint). If ModemManager is installed, also
install the udev rule described in [MODEMS.md](MODEMS.md#modemmanager-and-serial-consoles).

The database file and schema are created at the first start of the service.

## Running the command-line tool

The binary is not on the default `PATH`. `/etc/thugsred` is readable by root and the service
group only, and `smsgateway.env` by root only. Two practical options:

```sh
# an alias for interactive use
alias smsgw='sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway'

# or a symbolic link, then run it with sudo
sudo ln -s /opt/THUGSred-smsGateway/bin/thugsred-smsgateway /usr/local/bin/thugsred-smsgateway
```

Guidance:

- **Run it as root (`sudo`) by default.** `${NAME}` secret references are resolved from the
  environment and then from `smsgateway.env` next to the main configuration file. Only root
  can read that file, so a configuration that uses such references loads for root but fails
  for other users with "environment variable NAME is not set (and not found in
  smsgateway.env, which may need root to read)". See
  [CONFIGURATION.md](CONFIGURATION.md#secret-references).
- **Do not let root create the database.** `doctor` and the `db` commands open the database
  directly, and SQLite creates a missing file. If that happens as root before the service
  has ever started, the service cannot write to the file afterwards. Start the service once
  first, or fix the ownership with
  `sudo chown thugsred-smsgateway: /var/lib/thugsred-smsgateway/messages.db*`.
- **Restore as the service account** (`sudo -u thugsred-smsgateway ... db restore`), or
  restore as root and correct the ownership as above; `db restore` copies the backup into
  place with the ownership of whoever runs it. See [BACKUP-RESTORE.md](BACKUP-RESTORE.md).
- Commands that only talk to the daemon (`status`, `sms`, `modem`, `integrations`, `ussd`,
  `reload`, `capabilities`, `diagnostics`) still load the configuration first, to find the
  API address and token.
- `pdu`, `version` and `simulator` do not read the configuration.

## The systemd unit

[systemd/thugsred-smsgateway.service](../systemd/thugsred-smsgateway.service):

| Setting | Value |
| --- | --- |
| Type | `notify`; the daemon reports readiness after the HTTP listener is bound |
| User / group | `thugsred-smsgateway`, supplementary groups `dialout audio` |
| Start command | `/opt/THUGSred-smsGateway/bin/thugsred-smsgateway run --config /etc/thugsred/smsgateway.conf` |
| Reload | `kill -HUP $MAINPID` |
| Environment | `EnvironmentFile=-/etc/thugsred/smsgateway.env` (optional) |
| Restart | `on-failure`, 5 s delay; at most 5 starts in 300 s |
| Timeouts | start 60 s, stop 45 s |
| Watchdog | 120 s; liveness is reported only while the database answers |
| Directories | `StateDirectory`, `LogsDirectory` and `RuntimeDirectory` named `thugsred-smsgateway`, mode `0750` |
| Writable paths | The state, log and runtime directories plus `recordings` and `backups` under the prefix. Everything else is read-only (`ProtectSystem=strict`). |

If you move the database, recordings, attachments, backups or log files to other locations,
add matching `ReadWritePaths=` lines in a drop-in (`sudo systemctl edit
thugsred-smsgateway`), otherwise the sandbox blocks writes. The sandboxing options are listed
in [SECURITY.md](SECURITY.md#systemd-sandboxing).

## Uninstall and purge

```sh
sudo make uninstall      # removes the service and program files, keeps your data
sudo make purge          # removes everything, after confirmation
```

`make uninstall` ([scripts/uninstall.sh](../scripts/uninstall.sh)):

- disables and stops the service, removes `/etc/systemd/system/thugsred-smsgateway.service`;
- removes `bin`, `config`, `docs`, `scripts`, `systemd` and the top-level documentation files
  under the prefix;
- **keeps** `/etc/thugsred`, `/var/lib/thugsred-smsgateway`, `/var/log/thugsred-smsgateway`,
  and `recordings` and `backups` under the prefix, and lists them;
- keeps the service account.

`make purge` additionally deletes the configuration directory, the data and log directories,
the runtime directory, the whole prefix (including recordings and backups) and the service
account. It prints what will be deleted and asks you to type `delete-all-gateway-data`. For
unattended use set `PURGE_CONFIRM=delete-all-gateway-data`. This cannot be undone.

Safety checks in the uninstaller: it must run as root; `PREFIX` and `CONFDIR` must be clean
absolute paths and may not be system directories such as `/`, `/etc`, `/opt`, `/usr` or
`/var`; and a prefix that does not look like an installation of this project is refused.

## Debian package

```sh
make deb
```

Builds a release binary and writes `dist/thugsred-smsgateway_<version>_<arch>.deb` with
[scripts/build-deb.sh](../scripts/build-deb.sh). It needs `dpkg-deb`. The architecture is
`arm64` on an `aarch64` host and `amd64` otherwise.

Behaviour of the package:

- Program files are installed under `/opt/THUGSred-smsGateway`, the unit under
  `/lib/systemd/system/`.
- No configuration files are shipped. The post-installation script creates the service
  account, adds it to `dialout` and `audio`, creates `/etc/thugsred` and copies each example
  to its place **only if the file does not exist**, and creates the data, log, recordings and
  backup directories. Upgrades therefore cannot overwrite your settings.
- The service is not enabled or started automatically.
- The pre-removal script stops the service. Removing the package leaves configuration and
  data in place.
- Dependencies: `adduser`, `libc6`. `alsa-utils` is suggested.

The package is optional and has had less use than `make install`; treat it accordingly.

## Release archive

```sh
make package
```

Builds a release binary and the SBOM, then writes into `dist/`:

| File | Contents |
| --- | --- |
| `thugsred-smsgateway-<version>-linux-<arch>.tar.gz` | `bin/thugsred-smsgateway`, `config/`, `systemd/`, `scripts/`, `docs/`, `README.md`, `LICENSE`, `CHANGELOG.md`, `SECURITY.md`, `Makefile`; files are owned by uid/gid 0 inside the archive |
| `thugsred-smsgateway-<version>.sbom.cdx.json` | CycloneDX 1.5 software bill of materials derived from `Cargo.lock` (`make sbom`, needs `python3`) |
| `SHA256SUMS` | SHA-256 checksums of the archives, the SBOM and any `.deb` present in `dist/` |

To install from an unpacked archive, run the installer with the bundled binary:

```sh
tar -xzf thugsred-smsgateway-<version>-linux-<arch>.tar.gz
cd thugsred-smsgateway-<version>-linux-<arch>
sudo BINARY=bin/thugsred-smsgateway sh scripts/install.sh
```

## Cross-compilation

| Target | Make target | Needed on an x86_64 Debian host |
| --- | --- | --- |
| `aarch64-unknown-linux-gnu` | `make build-arm64` | `sudo apt install gcc-aarch64-linux-gnu libc6-dev-arm64-cross` and `rustup target add aarch64-unknown-linux-gnu` (or `apt install libstd-rust-dev-arm64-cross`) |
| `x86_64-unknown-linux-gnu` | `make build-amd64` | On a non-x86_64 host: `sudo apt install gcc-x86-64-linux-gnu libc6-dev-amd64-cross` and `rustup target add x86_64-unknown-linux-gnu` |

The output is `target/<triple>/release/thugsred-smsgateway`. On a host of the same
architecture the targets build natively with the system compiler as linker.

To install a cross-compiled binary on the target machine, copy the repository (or a release
archive) and the binary there and run:

```sh
sudo BINARY=/path/to/thugsred-smsgateway sh scripts/install.sh
```

The ARM64 build is produced and checked in CI, but **it has not yet been run on a Raspberry
Pi by the maintainers**.
