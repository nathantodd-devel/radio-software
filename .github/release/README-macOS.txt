Airspy Scanner for macOS (Apple silicon)

  scanner-ui   the desktop app
  scanner      the command-line scanner; run `./scanner --help`
  scanner-web  the scanner as a web page; run it and open
               http://localhost:1515/ (see `./scanner-web --help`)

Works with an Airspy R2 or Mini, or an RTL-SDR dongle (RTL2832U with an
R820T2 or R860 tuner, among others). You need the library for whichever you
have, from Homebrew:

  brew install airspy        # for an Airspy
  brew install librtlsdr     # for an RTL-SDR

These programs aren't signed. If macOS refuses to open them, clear the
download flag first:

  xattr -dr com.apple.quarantine .

Plug the receiver in and run ./scanner-ui.

Channels are kept in ~/Library/Application Support/airspy-scanner/channels.db.
For a portable install, create a folder called airspy-scanner beside the
programs: if it exists, the channels are kept there instead.
