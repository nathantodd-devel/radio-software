Airspy Scanner for Windows (64-bit)

  scanner-ui.exe   the desktop app
  scanner.exe      the command-line scanner; run `scanner --help` in a terminal

Works with an Airspy R2 or Mini, or an RTL-SDR dongle (RTL2832U with an
R820T2 or R860 tuner, among others).

Keep all the files in this folder together: the programs load the DLLs
beside them. Nothing else needs installing for an Airspy: Windows 10 and 11
set up its USB driver by themselves when you plug it in.

An RTL-SDR needs the WinUSB driver put on it once, with Zadig
(https://zadig.akeo.ie): plug the dongle in, run Zadig, choose "Bulk-In,
Interface (Interface 0)" and install WinUSB.

Plug the receiver in and run scanner-ui.exe.

Channels are kept in %APPDATA%\airspy-scanner\channels.db.
For a portable install, create a folder called airspy-scanner beside the
programs: if it exists, the channels are kept there instead.

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
