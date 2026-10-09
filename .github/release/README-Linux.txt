Airspy Scanner for Linux (x86_64)

  scanner-ui   the desktop app (Wayland or X11)
  scanner      the command-line scanner; run `./scanner --help`

Works with an Airspy R2 or Mini, or an RTL-SDR dongle (RTL2832U with an
R820T2 or R860 tuner, among others). You need, from your distribution, the
library for whichever you have, and aplay for audio:

  libairspy    Fedora: sudo dnf install airspyone_host
               Debian/Ubuntu: sudo apt install libairspy0
  librtlsdr    Fedora and Debian/Ubuntu: the rtl-sdr package
  aplay        Fedora and Debian/Ubuntu: the alsa-utils package

Those packages also install the udev rules that let you use the receiver
without being root. If an RTL-SDR won't open, the kernel's DVB-T driver may
be holding it: blacklist dvb_usb_rtl28xxu and plug it in again.

Plug the receiver in and run ./scanner-ui.

Channels are kept in ~/.local/share/airspy-scanner/channels.db.
For a portable install, create a folder called airspy-scanner beside the
programs: if it exists, the channels are kept there instead.
