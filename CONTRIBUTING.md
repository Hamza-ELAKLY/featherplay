# Contributing to Featherplay

Thanks for wanting to help. Featherplay's one rule is that it stays light: small,
quick to start, and frugal with memory and processor while it plays. A change is
welcome when it makes the player better without making it heavier.

Everyone taking part agrees to the [code of conduct](CODE_OF_CONDUCT.md).

## Reporting a bug

Open an issue with the **Bug report** form. The most useful details are:

- your Featherplay version (Settings, at the bottom) and Windows version;
- the link that misbehaved, if it is a public one;
- the exact message the app showed, if any ("Couldn't load …", "Skipped …");
- `data\crash.log` next to `featherplay.exe`, if the app closed by itself.

A video that plays nowhere, or that yt-dlp can't read, is usually fixed by a yt-dlp
update: Featherplay checks for one weekly, and Settings → **Check now** does it at once.

## Suggesting a feature

Open an issue with the **Feature request** form, and describe what you want to do,
not only the control you imagine. Features that cost memory or processor while idle,
or need a background service, are unlikely to fit.

Featherplay is a player, and stays one: features that save or download media, or that
get around YouTube's restrictions (private videos, age checks, paywalls), won't be
accepted. For open-ended ideas and
questions, use [Discussions](https://github.com/Hamza-ELAKLY/featherplay/discussions).

## Making a change

1. For anything bigger than a small fix, open an issue first, so we agree on the
   approach before you spend time on it.
2. Fork the repository and create a branch from `main` named for the change, like
   `fix-seek-after-pause` or `add-sleep-timer`.
3. Make the change, with tests where the code can be tested (see below).
4. Before you push, make sure these pass, as CI checks them on every pull request:

   ```
   cargo fmt --check
   cargo clippy --all-targets -- -D warnings
   cargo test --lib --bins
   ```

5. Open a pull request against `main`, describing what changed and how you tested it.
   CI runs the checks above and builds for Windows; the maintainer reviews it.

Commits are best kept focused, each with a message whose first line says what it does
in a few words ("Retry tracks that fail to load"), and a body explaining why, when
that isn't obvious. By contributing you agree that your work is released under the
project's licence, the [GNU General Public License, version 3 or later](LICENSE).

### How the code is laid out

| Where | What |
|---|---|
| `src/main.rs` | the window: wiring the interface to the player; the interface tests |
| `ui/app.slint` | the interface itself |
| `src/app.rs` | the player's logic: queue, loading, retries, sessions |
| `src/engine.rs`, `src/stream.rs`, `src/mp4.rs` | audio: streaming, decoding, seeking |
| `src/extract.rs` | talking to yt-dlp |
| `src/queue.rs`, `src/store.rs` | the queue's rules, and what is saved |
| `src/update.rs` | updating Featherplay itself |
| `examples/` | programs that drive the real player against YouTube, without a window |
| `scripts/` | release packaging, licence notices, the README banner |

### Building and testing

You need Rust (stable). On Linux, also the system libraries listed in the README.
A Windows build can be cross-compiled from Linux with `mingw-w64` and
`cargo build --release --target x86_64-pc-windows-gnu`.

`cargo test --lib --bins` runs the unit tests and the interface tests, which drive the
window headlessly (pointer, keyboard, drag) with the real renderer;
`FEATHERPLAY_SNAPSHOTS=<folder>` makes some of them save what the window showed.
If you change playback, also try the programs under `examples/` against real links,
and watch memory in Task Manager while a track plays: it should stay around 40 MB.

## Releasing (maintainers)

1. Update the version in `Cargo.toml`, and add a `## <version>` section at the top of
   `CHANGELOG.md` saying what changed, for users.
2. Commit, then tag and push: `git tag v<version> && git push origin main v<version>`.
3. The **Release** workflow builds the portable zip, the updater's executable and
   their checksums, and publishes them as a GitHub release with the changelog
   section as its notes. Within a day, everyone's Featherplay offers the update.

A tag with a suffix, like `v0.3.0-rc1`, becomes a pre-release, which the in-app
updater never offers: use it to try a release before it reaches everyone.

Releases are signed: the workflow signs `SHA256SUMS.txt` with the private key in the
repository secret `FEATHERPLAY_SIGNING_KEY`, and the app installs an update only if
that signature matches the public key in `src/update.rs` (`RELEASE_KEY`). The key pair
was made with `cargo run --example release_keygen -- <file>`. Never commit the private
key; if it's ever exposed, make a new pair, put the new public key in the app, and
release that version signed with the old key, so existing copies can still update.
