# Raspberry Pi 4

> **Not yet verified by the maintainers.** The gateway builds for `aarch64`, and the ARM64
> build is produced in CI, but it has not been run on a Raspberry Pi. The GPIO UART wiring
> described here has not been tested either: the one module checked so far was attached
> through USB to an x86_64 host. Treat this page as guidance and please report what you find
> ([CONTRIBUTING.md](../CONTRIBUTING.md#reporting-modem-compatibility)).

Target: Raspberry Pi 4 with Raspberry Pi OS 64-bit and a GSM HAT such as the Waveshare
GSM/GPRS/GNSS HAT (SIMCom SIM868).

- [Checklist](#checklist)
- [Build or copy the binary](#build-or-copy-the-binary)
- [Choosing the serial connection](#choosing-the-serial-connection)
- [Enabling the GPIO UART](#enabling-the-gpio-uart)
- [HAT jumper positions](#hat-jumper-positions)
- [Power-on with PWRKEY](#power-on-with-pwrkey)
- [Power supply and antenna](#power-supply-and-antenna)
- [2G network availability](#2g-network-availability)
- [Serial permissions](#serial-permissions)
- [ModemManager](#modemmanager)
- [Verify](#verify)

## Checklist

1. Install the gateway ([Build or copy the binary](#build-or-copy-the-binary)).
2. Decide between USB and GPIO UART and set the HAT jumpers to match.
3. For the GPIO UART: enable the serial port and disable the serial login console.
4. Set `modem.device` in `/etc/thugsred/smsgateway.conf`.
5. Start the service and press PWRKEY.
6. Run `doctor`.

The installer never changes boot settings, never edits `config.txt` or the kernel command
line, and never enables or disables other services. The steps in this document that do so are
manual steps for the administrator.

## Build or copy the binary

**On the Pi:**

```sh
sudo apt install build-essential
# install Rust from https://rustup.rs or: sudo apt install cargo
make deps
make release
sudo make install
```

**Cross-compiled on an x86_64 host:**

```sh
sudo apt install gcc-aarch64-linux-gnu libc6-dev-arm64-cross
rustup target add aarch64-unknown-linux-gnu
make build-arm64
```

Copy the repository (or a release archive) and
`target/aarch64-unknown-linux-gnu/release/thugsred-smsgateway` to the Pi, then:

```sh
sudo BINARY=/path/to/thugsred-smsgateway sh scripts/install.sh
```

See [INSTALLATION.md](INSTALLATION.md) for what the installer does.

## Choosing the serial connection

| Connection | Device | Notes |
| --- | --- | --- |
| USB cable to the HAT's USB port | `/dev/ttyUSB0`, or a stable name under `/dev/serial/by-path/` | The HAT's USB-UART bridge appears as a USB serial device. No boot settings are needed. This is the connection type that has been checked (on an x86_64 host). |
| GPIO header UART | `/dev/ttyS0` (the gateway's default), also reachable as `/dev/serial0` | Needs the serial port enabled and the login console removed from it. Unverified. |

`/dev/serial0` is a symbolic link that Raspberry Pi OS points at the primary UART; which
physical UART that is depends on the Pi model and on whether Bluetooth uses the other one.
Check the Raspberry Pi documentation for your model. Either name works as `modem.device`:

```toml
[modem]
device = "/dev/serial0"
baud_rate = 115200
```

## Enabling the GPIO UART

These are manual steps. Run them yourself and reboot afterwards.

```sh
sudo raspi-config
```

1. *Interface Options* → *Serial Port*.
2. "Would you like a login shell to be accessible over serial?" → **No**.
3. "Would you like the serial port hardware to be enabled?" → **Yes**.
4. Finish and reboot.

Why both answers matter: a login console on the port competes with the gateway for the same
serial line. `doctor` reports both variants of this conflict:

- `serial-getty@<tty>.service` is active on the port
  (fix: `sudo systemctl disable --now serial-getty@ttyS0.service`);
- the kernel command line contains `console=<tty>` for the port (fix: remove it with
  `raspi-config` as above).

## HAT jumper positions

GSM HATs with both a USB port and a GPIO UART connection have jumpers (or a switch) that
select which of the two is wired to the module's serial port. The position must match the
connection you chose:

- **USB position** when the gateway reaches the module through the HAT's USB port;
- **GPIO/UART position** when it reaches the module through the Pi's header.

With the jumpers in the wrong position the serial device opens, but the module never answers,
which looks exactly like a module that is switched off. The gateway's hint for this situation
mentions the jumpers for that reason.

The labelling and layout of the jumpers differ between boards and board revisions. Check the
Waveshare documentation for your exact board revision rather than relying on a description
here.

## Power-on with PWRKEY

The module does not power on by itself when the Pi boots or when the HAT is attached. Hold
the **PWRKEY** button for about two seconds until the NET LED starts blinking.

The gateway reports an unpowered module as state `DISCONNECTED` with the error
`no response to AT commands (module powered off?)` and a hint, keeps probing, and connects on
its own once the module answers. See
[MODEMS.md](MODEMS.md#power-on-and-the-pwrkey-hint).

Some HATs also route PWRKEY to a GPIO pin so that it can be pulsed from software. The gateway
does not do this; if you want automatic power-on after boot you have to arrange it outside
the gateway.

## Power supply and antenna

- GSM modules draw short, high current peaks while transmitting. A weak supply or a thin USB
  cable can make the module reset or the Pi brown out. Use a supply with comfortable headroom
  for the Pi plus the HAT and follow the HAT manufacturer's requirements.
- Attach the GSM antenna before powering the module on. Without it the module usually stays
  unregistered or shows a very weak signal.
- The GNSS antenna is separate; it is only needed if you enable `[gnss]`.
- A module that restarts unexpectedly shows up in the system log as "modem restarted" or
  "modem powered down" followed by a reconnect.

## 2G network availability

> The SIM868 and the other modules covered by the `simcom-2g` profile are GSM/GPRS-only.
> They need a 2G network. Many operators have reduced or shut down their 2G networks, and
> availability differs by country and operator. Check that your operator still offers 2G
> where the gateway will run, and that your SIM is allowed to use it, before buying hardware.

A module without a usable network stays in state `REGISTERING` with a registration of
`searching`, `not_registered` or `denied`.

## Serial permissions

The service runs as the user `thugsred-smsgateway`. The installer adds it to the `dialout`
group, which owns serial devices on Raspberry Pi OS and Debian. Check with:

```sh
ls -l /dev/ttyS0 /dev/ttyUSB0
id thugsred-smsgateway
```

If the device belongs to another group, add the service user to that group and restart the
service. `doctor` prints the exact `usermod` command when neither the current user nor the
service user can open the device.

## ModemManager

If ModemManager is installed it may probe serial modems and disturb the gateway's
conversation with the module. `doctor` warns when `ModemManager.service` is active.

For USB-attached modules install the provided udev rule, which tells ModemManager to ignore
common USB-UART bridges (Silicon Labs CP210x, QinHeng CH340/CH341, FTDI FT232):

```sh
sudo cp /opt/THUGSred-smsGateway/scripts/99-thugsred-smsgateway.rules /etc/udev/rules.d/
sudo udevadm control --reload
sudo udevadm trigger
```

Remove the lines for adapters that ModemManager should keep managing on that machine. The
rule matches USB devices, so it does not cover the GPIO UART; if ModemManager interferes with
`/dev/ttyS0` and nothing else on the Pi needs it, stop and disable it:

```sh
sudo systemctl disable --now ModemManager.service
```

## Verify

```sh
sudo systemctl enable --now thugsred-smsgateway
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway doctor
sudo /opt/THUGSred-smsGateway/bin/thugsred-smsgateway modem info
```

`doctor` reports the architecture (`aarch64` is accepted), the serial device, conflicting
services, and the modem, SIM and network state. If something does not work, see
[TROUBLESHOOTING.md](TROUBLESHOOTING.md).
