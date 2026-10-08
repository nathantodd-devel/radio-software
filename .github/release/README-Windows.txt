Airspy Scanner for Windows (64-bit)

  scanner-ui.exe   the desktop app
  scanner.exe      the command-line scanner; run `scanner --help` in a terminal

Keep all the files in this folder together: the programs load the DLLs
beside them. Nothing else needs installing. Windows 10 and 11 set up the
Airspy's USB driver by themselves when you plug it in.

Plug the Airspy in and run scanner-ui.exe.

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
