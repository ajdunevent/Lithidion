# Lithidion conventions

- App ID: `net.dunevent.Lithidion`
- License: GPL-3.0-or-later
- Stack: Rust, GTK4, libadwaita; target FuriLabs FLX1s, FuriOS (Debian forky), Phosh, BlueZ 5.85
- Workspace: `crates/pebble-protocol` (protocol, no I/O), `crates/lithidion` (app)
- The official Pebble app (GPL-3.0) and PebbleOS (Apache-2.0) are read for facts, recorded in our
  own words in `docs/PROTOCOL.md` with their source path and commit, and implemented from those
  notes in our own structure and naming; no code is copied or translated line by line
- Code comments follow "Comment Style Guide 2026-09-21"
- Source on the project forge: https://dev.dunevent.net/aj/Lithidion (git, `main`); the user commits and pushes
- Deliveries: zip files containing only changed or new files, with directory structure preserved under the project root `Lithidion/`. Deleted files are listed in the message.
- `.no-sync` files list the direct children of their folder that device sync should skip (build output), one name per line, like `.hidden`. A folder whose own children need skipping gets its own `.no-sync`. Keep them current when a build step adds an output folder, together with `.gitignore` and the manifest's `skip` list.
- README is a landing page: features and how to use the app. Building, layout, and tests go in `docs/DEVELOPMENT.md`
- Docs do not name the hardware Lithidion is tested on, or what devices the user owns
- UI: four fixed tabs (Watch, Apps, Notifications, Health); settings stored on a watch live under the Watch tab, phone-side settings in Preferences; Advanced in the main menu; profiles own watches and their data; one watch active at a time
- Dates and times shown to the user come from `ui::locale` (the locale's own formats, in the followed time zone); never a fixed `strftime` pattern
- About dialog and metainfo developer: "Lithidion contributors"; donate link https://dunevent.net/donate
