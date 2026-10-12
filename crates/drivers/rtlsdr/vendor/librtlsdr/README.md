# librtlsdr

The RTL-SDR library, from the RTL-SDR Blog's fork,
<https://github.com/rtlsdrblog/rtl-sdr-blog>, at tag `V1.4.0` (`src/librtlsdr.c`,
the tuner drivers beside it, and `include/`). It is the version the Windows
release bundles as a DLL.

It is compiled into the driver on Android only, where there is no system
librtlsdr to load. Its licence is the GNU General Public License, version 2
or any later version: see `COPYING` and the top of each file.

## What is changed

One addition, marked `DMPS` in the source: `rtlsdr_open_fd`, which opens the
device the operating system has already opened as a file descriptor. Android
doesn't let an app open USB devices itself; the system does it and hands
over a descriptor. To make room for it, the body of `rtlsdr_open` became
`rtlsdr_open_index_or_fd`, which both now call. Nothing else differs from
upstream.
