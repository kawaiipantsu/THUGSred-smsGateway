# Backup, restore and upgrade

- [What to back up](#what-to-back-up)
- [SQLite: online backup](#sqlite-online-backup)
- [Verifying a backup](#verifying-a-backup)
- [SQLite: restore](#sqlite-restore)
- [MySQL and MariaDB](#mysql-and-mariadb)
- [Files: configuration, recordings, attachments](#files-configuration-recordings-attachments)
- [Upgrade procedure](#upgrade-procedure)
- [Rollback](#rollback)

Commands are written with the full path of the installed binary through a variable:

```sh
BIN=/opt/THUGSred-smsGateway/bin/thugsred-smsgateway
```

## What to back up

| What | Default location | Contains | How |
| --- | --- | --- | --- |
| Database | `/var/lib/thugsred-smsgateway/messages.db` (SQLite), or your MySQL/MariaDB database | Messages, queue, events, deliveries, call log, audit trail, SIM PIN attempt counters | `db backup` (SQLite) or `mysqldump` |
| Configuration | `/etc/thugsred/` | All `.conf` files, `smsgateway.env` (secrets), custom modem profiles, any secret files you reference | File copy, as root |
| Recordings | `/opt/THUGSred-smsGateway/recordings/` | Call recordings (if used) | File copy |
| Attachments | `/var/lib/thugsred-smsgateway/attachments/` | Downloaded MMS content (if used) | File copy |
| systemd drop-ins | `/etc/systemd/system/thugsred-smsgateway.service.d/` | Your overrides (if any) | File copy |

Not needed: program files under `/opt/THUGSred-smsGateway` (reinstall them), logs (unless
you want them), `/run/thugsred-smsgateway`.

Backups contain personal data (message text, numbers) and, in the configuration, secrets.
Store and transport them accordingly.

Do **not** back up a live SQLite database by copying `messages.db`: with write-ahead logging
the file alone can be inconsistent. Use `db backup`.

## SQLite: online backup

`db backup` creates a consistent copy while the daemon keeps running (`VACUUM INTO`), then
opens the copy read-only and runs an integrity check. If the check fails the command fails.

```sh
sudo $BIN db backup
# backup written and verified: /opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db
```

| Way | Command | Notes |
| --- | --- | --- |
| Command line | `sudo $BIN db backup [--dir <directory>]` | Default directory: `paths.backups` |
| Makefile | `sudo make backup` | Runs the installed binary with the installed configuration |
| API | `POST /api/v1/database/backup` | Administrative; written by the daemon into `paths.backups`; returns `{"path": "...", "verified": true}` |

Details:

- File name: `messages-<UTC timestamp>.db`, for example `messages-20261124T110000Z.db`.
- The copy is compacted, so it can be smaller than the live file.
- The target directory must be writable by whoever runs the command: the service account for
  the API call, root for `sudo`. With the shipped unit the daemon can write only to its own
  directories; keep the default `paths.backups` for API-triggered backups.
- The backup needs free space for a full copy of the database.
- **Old backups are never deleted automatically.** Prune them yourself, and copy them off
  the host: a backup on the same disk does not survive the disk.
- On MySQL/MariaDB all three ways fail with "built-in backup supports SQLite only".

Scheduled backup with a systemd timer or cron, using the API so that the file is owned by
the service account:

```sh
curl -fsS -X POST http://127.0.0.1:8080/api/v1/database/backup
```

(add `-H 'Authorization: Bearer <admin token>'` when tokens are configured).

## Verifying a backup

```sh
sudo $BIN db verify /opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db
# /opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db: integrity check passed
```

`db verify <file>` opens the file read-only, runs SQLite's integrity check and confirms that
it contains the gateway's schema table. It does not need the daemon and does not touch the
live database. Verify a backup again after copying it to other storage, and before restoring
it.

`db verify` without a file checks the **live** database instead:

```sh
sudo $BIN db verify
# engine: sqlite  schema version: 1  result: ok
```

## SQLite: restore

Restoring replaces the whole database: everything received or sent after the backup was
taken is lost.

```sh
sudo systemctl stop thugsred-smsgateway

sudo -u thugsred-smsgateway $BIN db restore \
    /opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db --yes

sudo systemctl start thugsred-smsgateway
sudo $BIN status
```

What `db restore` does:

1. Refuses to run unless `database.driver` is `sqlite`.
2. Verifies the backup file (as `db verify <file>` does); an unusable file is rejected.
3. Without `--yes`, stops here and tells you what it would replace.
4. Moves the current database aside as `messages.db.before-restore-<UTC timestamp>` in the
   same directory. Nothing is deleted.
5. Removes the stale `-wal` and `-shm` companion files.
6. Copies the backup into place.

Important:

- **Stop the daemon first.** The command does not check whether it is running; restoring
  under a running daemon corrupts its view of the database.
- **Run it as the service account**, as shown. The restored file belongs to whoever runs the
  command; a file owned by root cannot be written by the service. If you had to run it as
  root, fix the ownership before starting the service:

  ```sh
  sudo chown thugsred-smsgateway: /var/lib/thugsred-smsgateway/messages.db
  ```

  Running as the service account requires that the configuration loads for that account.
  It cannot read `smsgateway.env` (mode `0600`, root), so if your configuration uses
  `${NAME}` references for enabled settings, run the restore as root and fix the ownership
  as above.
- The backup file must be readable by the account that runs the restore.
- Messages that were waiting in the queue at backup time are in the restored database and
  **will be sent** when the service starts. If they were already sent after the backup was
  taken, they go out a second time. Review `GET /api/v1/queue` right after starting, or
  start with `sms.send_enabled = false`, cancel what should not go out, and enable sending
  again.
- If the backup comes from an older release, pending migrations are applied at start (with
  `database.auto_migrate = true`).
- Remove the `messages.db.before-restore-*` file once you are satisfied.

### `make restore`

```sh
make restore BACKUP=/opt/THUGSred-smsGateway/backups/messages-20261124T110000Z.db
```

asks you to type `restore`, stops the service, runs `db restore ... --yes` as the service
account and starts the service again. **Run it as a normal user who may use `sudo`, not as
root and not through `sudo make`.** The target relies on `sudo -u` to switch to the service
account; started as root it has no `sudo` prefix, the restore step does not run, and the
service is simply restarted with the old database. When in doubt use the manual steps above.

## MySQL and MariaDB

The built-in backup and restore commands are SQLite-only. Use the database server's tools.
Replace the placeholders; the examples read the password interactively (`-p`).

Backup:

```sh
mysqldump --single-transaction --routines --triggers \
    -h db.example.invalid -u smsgateway -p smsgateway \
    | gzip > smsgateway-$(date -u +%Y%m%dT%H%M%SZ).sql.gz
```

`--single-transaction` gives a consistent snapshot of the InnoDB tables without blocking the
running gateway. With MariaDB the client may be called `mariadb-dump`.

Check the dump:

```sh
gzip -t smsgateway-<timestamp>.sql.gz && zcat smsgateway-<timestamp>.sql.gz | tail -n 1
# a complete dump ends with a line like: -- Dump completed on ...
```

The only real verification is a test restore into a scratch database:

```sh
mysql -h db.example.invalid -u <admin user> -p -e 'CREATE DATABASE smsgateway_verify CHARACTER SET utf8mb4'
zcat smsgateway-<timestamp>.sql.gz | mysql -h db.example.invalid -u <admin user> -p smsgateway_verify
mysql -h db.example.invalid -u <admin user> -p -e 'SELECT COUNT(*) FROM smsgateway_verify.messages; DROP DATABASE smsgateway_verify'
```

Restore:

```sh
sudo systemctl stop thugsred-smsgateway

mysql -h db.example.invalid -u <admin user> -p -e \
    'DROP DATABASE smsgateway; CREATE DATABASE smsgateway CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci'
zcat smsgateway-<timestamp>.sql.gz | mysql -h db.example.invalid -u <admin user> -p smsgateway

sudo systemctl start thugsred-smsgateway
```

Take a fresh dump of the current state before dropping anything. The same caution about
queued messages applies as for SQLite. Grants are stored outside the database and survive
`DROP DATABASE`.

`db verify` on this engine only confirms connectivity and the schema version.

## Files: configuration, recordings, attachments

```sh
sudo tar -C / -czf gateway-config-$(date -u +%Y%m%d).tar.gz etc/thugsred
sudo tar -C / -czf gateway-files-$(date -u +%Y%m%d).tar.gz \
    opt/THUGSred-smsGateway/recordings var/lib/thugsred-smsgateway/attachments
```

The configuration archive contains `smsgateway.env` and possibly a SIM PIN: protect it like
the secrets themselves.

Restoring files: unpack them to the same locations and restore ownership and modes
([INSTALLATION.md](INSTALLATION.md#what-the-installer-creates)): configuration
`root:thugsred-smsgateway` `0640` (`smsgateway.env` `root:root` `0600`); recordings and
attachments owned by `thugsred-smsgateway`. The database refers to recordings and
attachments by absolute path, so they must return to the paths they had.

Moving to a new host: install the gateway there, stop the service, restore configuration,
database and files, check `modem.device` (device names differ between machines), run
`check-config`, start the service. SIM PIN attempt counters travel with the database.

## Upgrade procedure

```sh
cd THUGSred-smsGateway
git pull
make release                 # as a normal user
sudo make upgrade
```

Read [CHANGELOG.md](../CHANGELOG.md) first for anything that needs attention.

What `make upgrade` does, in order:

1. Refuses to run without `target/release/thugsred-smsgateway`.
2. Runs `db backup` with the **currently installed** binary. If that fails (for example on
   MySQL/MariaDB, where the built-in backup does not apply) it prints
   "note: automatic backup skipped" and continues. **On MySQL/MariaDB take a `mysqldump`
   yourself before upgrading.**
3. Runs `make install`: replaces the program files and the unit file. Configuration and data
   are not touched. The running daemon keeps using the old binary until it is restarted.
4. Runs `check-config` with the **new** binary. If the configuration is not valid for the new
   release, the upgrade stops here: the new files are installed, but the old daemon is still
   running. Fix the configuration and run `sudo systemctl restart thugsred-smsgateway`.
5. Restarts the service if it was running; otherwise it tells you to start it.

Schema migrations are applied when the new daemon starts (`database.auto_migrate = true`),
or by `sudo $BIN db migrate` beforehand when you have disabled automatic migration.

After the upgrade:

```sh
sudo $BIN version
sudo $BIN doctor
sudo $BIN status
```

Notes:

- `make upgrade` must run as root (the install step requires it); build as a normal user
  first.
- With a non-default prefix or configuration directory pass the same `PREFIX=` and
  `CONFDIR=` as at installation, and `CONFIG=` for the configuration file.
- Messages that are being transmitted at the moment of the restart may end up as
  `submission_unknown`; queued messages and pending integration deliveries survive the
  restart. Check `GET /api/v1/queue?status=submission_unknown` afterwards.
- New configuration options take their defaults; your files are never rewritten. Compare
  with the examples in `/opt/THUGSred-smsGateway/config/` for new settings.
- Debian package users: install the new package with `dpkg -i`; take a backup first, as the
  package does not do it for you.

## Rollback

Going back to the previous release has two parts: the binary and, if the upgrade changed the
schema, the database.

1. Stop the service.

   ```sh
   sudo systemctl stop thugsred-smsgateway
   ```

2. Reinstall the previous release: check out its tag, `make release`, `sudo make install`
   (or reinstall its package or archive). Keeping the previous release archive or `.deb`
   around makes this quick.

3. If the newer release applied a schema migration, restore the backup taken before the
   upgrade ([SQLite](#sqlite-restore), [MySQL/MariaDB](#mysql-and-mariadb)). Migrations only
   move forward; an older binary does not understand a newer schema and there are no
   down-migrations. Everything received and sent since the upgrade is lost from the
   database in that case; export it first if you need it:

   ```sh
   curl -sS -OJ 'http://127.0.0.1:8080/api/v1/history/export?format=json&from=<upgrade date>'
   ```

   (run this while the new release is still running, before step 1).

   If no migration was applied (compare the schema version in `status` before and after),
   keep the database as it is.

4. Start the service and check `doctor` and `status`.

Release 0.1.0 is the first release: it has a single schema version (1) and nothing to roll
back to.
