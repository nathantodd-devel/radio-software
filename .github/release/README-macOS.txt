Airspy Scanner for macOS (Apple silicon)

  scanner-ui   the desktop app
  scanner      the command-line scanner; run `./scanner --help`

You need libairspy, from Homebrew:

  brew install airspy

These programs aren't signed. If macOS refuses to open them, clear the
download flag first:

  xattr -dr com.apple.quarantine .

Plug the Airspy in and run ./scanner-ui.

Channels are kept in ~/Library/Application Support/airspy-scanner/channels.db.
For a portable install, create a folder called airspy-scanner beside the
programs: if it exists, the channels are kept there instead.
