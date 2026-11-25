# Developing Lithidion

## Build and run

```sh
sudo apt install build-essential pkg-config libdbus-1-dev libgtk-4-dev libadwaita-1-dev
cargo build --release
target/release/lithidion
```

Lithidion is two programs. `lithidion-service` keeps the watch connected and owns the database;
`lithidion` is the window, and starts the service from beside itself if it is not running (under
Flatpak, D-Bus starts it). `lithidion --hidden` only starts the service, as old autostart entries
do. The service logs to its own output: set `RUST_LOG=lithidion_core=debug` before starting it to
log every Pebble Protocol frame sent and received. Data lives in
`~/.local/share/lithidion/lithidion.db`.

## Service

The service owns `net.dunevent.Lithidion.Service` on the session bus, at
`/net/dunevent/Lithidion/Service`, interface `net.dunevent.Lithidion.Service1`. Commands and events
are the backend's own (`backend::Command`, `backend::Event`), as JSON:

| Member | |
|---|---|
| `Hello(s build) → (s build, t seq, aay snapshot)` | The service's build, and the latest event of each kind that describes state, current to `seq` |
| `Command(ay)` | One command; `Shutdown` quits the service |
| `Quit()` | Closes the watch connection and exits |
| signal `Event(t seq, ay)` | Each event, numbered |

A window subscribes to `Event` before saying hello and drops events numbered at or below the
snapshot's. The build changes whenever `lithidion-core` or `pebble-protocol` does; a window from
another build quits the service and starts its own. Closing the window leaves the service running
unless Run in Background is off; Quit stops both.

Under Flatpak the service sets its status in the Background portal (version 2 and later) from the
connection and firmware updates, for the desktop's list of background apps.

## Flatpak

`build-aux/dev.sh` asks for the architecture (this machine, the other one of x86_64 and aarch64,
or both), whether to write a `.flatpak` file to `dist/`, install it for the current user, or both,
and which build profile to use: fast release (the `release-dev` profile: optimised, no LTO,
incremental), debug, or release as published. It keeps the crate cache and each profile's compiled
output in `.flatpak/`, so a second build recompiles only what changed. Building for the other
architecture needs QEMU user emulation registered with binfmt_misc (`qemu-user-static`), and is
slow.

`build-aux/cargo-sources.json` lists every crate for the offline build. Regenerate it whenever
`Cargo.lock` changes, with `python3 build-aux/cargo-sources.py` from the project root (Python 3.11
or later, standard library only). Under Flatpak the
database is in `~/.var/app/net.dunevent.Lithidion/data/lithidion/`, and start at login goes
through the Background portal.

## Distrobox

Inside a Distrobox container, Lithidion finds the host's Bluetooth service on its own: distrobox
leaves the host's system bus out of the container, so it uses it from `/run/host`. The container
needs the build packages (on Fedora: `gcc pkgconf-pkg-config dbus-devel gtk4-devel
libadwaita-devel`). A watch paired with the phone has to be paired again on the computer.

## Layout

| Path | Contents |
|---|---|
| `crates/pebble-protocol` | Pebble Protocol without I/O: PPoGATT state machine, framing, field encodings, session-start messages, hardware tables, Bluetooth UUIDs and records |
| `crates/lithidion-core` | Everything but the window: BlueZ transport, SQLite store, backend thread, PebbleKit JS, and the service's bus interface (`service.rs`) |
| `crates/lithidion-service` | The background service: the backend on the session bus, without GTK |
| `crates/lithidion` | The window, and its client for the service (`client.rs`) |
| `crates/lithidion-core/src/backend` | `bluetooth` (BlueZ helpers), `link` (connect, pair, pick the transport), `gatt_server` (the phone's forward PPoGATT service), `session` (pump, session start, unprompted messages), `mod` (commands, events, the supervisor that keeps the active watch connected) |
| `crates/lithidion/src/ui` | One file per screen or dialog |
| `tools/voiceprobe` | Speech-to-text probe: how fast and how well a model transcribes on this machine; development builds only; see its README |
| `data/` | Desktop file, metainfo, the service's D-Bus activation file, the app icons (drawn by `data/icons/app-icon.py`), and the symbolic icons, compiled into the binary from `icons.gresource.xml` by `crates/lithidion/build.rs` (needs `glib-compile-resources`, which the GTK development packages bring) |
| `build-aux/` | Flatpak manifest, vendored crate list and `dev.sh` |
| `.forgejo/workflows/`, `.github/workflows/` | Tagging on the forge, building and releasing on GitHub |
| `docs/PROTOCOL.md` | The protocol as Lithidion uses it, with sources and confidence for each fact |
| `docs/project/` | Conventions and the backlog |
| `.no-sync` | Build output that device sync should skip: one name per line, direct children of this folder only |

## Tests

```sh
cargo test
```

`crates/pebble-protocol/tests/stream.rs` drives a PPoGATT session against a simulated watch over a
link that drops packets.

Tests marked `#[ignore]` need something outside the build and say what in their comments: a
display (`xvfb-run`), a D-Bus session (`dbus-run-session`), a PBW (`PKJS_PBW`), or, for
`dates_follow_the_locale`, the en_US, en_GB, and de_DE locales (`LOCPATH` may point at ones built
with `localedef -i en_US -f UTF-8 <dir>/en_US.UTF-8`).

The dictation tests (`watch_audio_round_trip`, `session_from_setup_to_result`) need the speech
model at `LITHIDION_TEST_MODEL` and a 16 kHz mono WAV at `LITHIDION_TEST_WAV`; the transcribe.cpp
repository's `samples/jfk.wav` works. Run `session_from_setup_to_result` on its own: it points
the data folder at a temporary one.

The speech engine (ggml) tunes itself to the building processor unless `TRANSCRIBE_CMAKE_ARGS`
fixes a baseline; `build-aux/dev.sh` and the manifest set one per architecture, and a plain
`cargo build` is tuned to this machine, which is fine for running here.

## Releasing

Change the version in `Cargo.toml`, add a `<release>` at the top of `<releases>` in
`data/net.dunevent.Lithidion.metainfo.xml`, and push to `main`. `.forgejo/workflows/tag.yml`
then tags the commit `v<version>` on the forge, or fails without tagging if the two versions
differ. The push mirror carries the tag to GitHub, where `.github/workflows/release.yml` runs the
[FrogPak releaser](https://github.com/FrogPak/releaser): it builds
`net.dunevent.Lithidion-x86_64.flatpak` and `net.dunevent.Lithidion-aarch64.flatpak` and
publishes a GitHub release with both, which FuriHub and AJHub pick up.

`Cargo.lock` does not need updating by hand for a version change; the Flatpak build updates the
workspace crates' versions in it offline.

Tags must not be made on GitHub: the mirror deletes any tag the forge does not have, and GitHub
then turns that tag's release into a draft, which the Ponds drop.
