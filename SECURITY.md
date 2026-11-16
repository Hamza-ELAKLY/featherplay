# Security

Featherplay downloads and runs code in two places, so these are where security
matters most:

- **Self-update** downloads a new `featherplay.exe` from this repository's latest
  stable release, over HTTPS. It installs it only if the release's `SHA256SUMS.txt` is
  signed with the project's release key (Ed25519; the public key is built into the app,
  the private key is held only by the release workflow), and the download matches the
  SHA-256 listed there. A release published any other way, even from the project's own
  GitHub account, is refused.
- **yt-dlp** is run as a separate program and kept up to date with its own
  `--update`, which downloads yt-dlp's official releases.

If you find a vulnerability, in these or anywhere else, please don't open a public
issue. Report it privately through GitHub: **Security → Report a vulnerability** on
this repository. You'll get an answer as soon as possible, and credit if you'd like.

Only the latest release is supported with fixes.
