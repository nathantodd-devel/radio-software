Airspy Scanner for Linux (x86_64)

  scanner-ui   the desktop app (Wayland or X11)
  scanner      the command-line scanner; run `./scanner --help`

You need, from your distribution:

  libairspy    Fedora: sudo dnf install airspyone_host
               Debian/Ubuntu: sudo apt install libairspy0
  aplay        for audio; Fedora and Debian/Ubuntu: the alsa-utils package

Those packages also install the udev rule that lets you use the Airspy
without being root. Plug the Airspy in and run ./scanner-ui.

Channels are kept in ~/.local/share/airspy-scanner/channels.db.
For a portable install, create a folder called airspy-scanner beside the
programs: if it exists, the channels are kept there instead.
