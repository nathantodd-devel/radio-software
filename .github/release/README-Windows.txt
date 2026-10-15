Digital Mobile Police Scanner (DMPS) for Windows (64-bit)

  scanner-ui.exe   the desktop app
  scanner.exe      the command-line scanner; run `scanner --help` in a terminal
  scanner-web.exe  the scanner as a web page; run it and open
                   http://localhost:1515/ (see `scanner-web --help`)

Works with an Airspy R2 or Mini, a HackRF, or an RTL-SDR dongle (RTL2832U with an
R820T2 or R860 tuner, among others).

Keep all the files in this folder together: the programs load the DLLs
beside them. Nothing else needs installing for an Airspy: Windows 10 and 11
set up its USB driver by themselves when you plug it in.

An RTL-SDR needs the WinUSB driver put on it once, with Zadig
(https://zadig.akeo.ie): plug the dongle in, run Zadig, choose "Bulk-In,
Interface (Interface 0)" and install WinUSB.

A HackRF needs nothing installed: its library is built into the programs,
and Windows sets up its USB driver by itself.

Plug the receiver in and run scanner-ui.exe.

Channels are kept in %APPDATA%\dmps\channels.db.
For a portable install, create a folder called dmps beside the
programs: if it exists, the channels are kept there instead.

To update, run `scanner update` in a terminal. If you used the installer and
left automatic updates on, a scheduled task does it for you; update.log in
this folder says what it did.

Bundled libraries:
  airspy.dll, libusb-1.0.dll, pthreadVC2.dll
      from the official Airspy host tools 1.0.10,
      https://github.com/airspy/airspyone_host (libairspy: BSD 3-clause;
      libusb and pthreads-win32: LGPL 2.1)
  msvcr100.dll
      the Microsoft Visual C++ 2010 runtime, which pthreadVC2.dll needs
  rtlsdr.dll
      from the RTL-SDR Blog's rtl-sdr 1.4.0,
      https://github.com/rtlsdrblog/rtl-sdr-blog (GPL 2.0 or later)
  vcruntime140.dll
      the Microsoft Visual C++ runtime, which rtlsdr.dll needs

Built into the programs:
  libhackrf 2026.01.3, https://github.com/greatscottgadgets/hackrf
      Copyright (c) 2012-2022 Great Scott Gadgets and others; BSD 3-clause.
      Its licence is in host/libhackrf/src/hackrf.c there.
  libusb, https://libusb.info (LGPL 2.1)
