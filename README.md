# Lithidion

**Your Pebble watch, on Linux. No cloud, no account.**

Lithidion talks to your Pebble watch directly over Bluetooth from a Linux phone, tablet, or
computer. It is built with GTK4 and libadwaita, and fits a phone screen as well as a desktop
window.

## What it does

**Watch**: your watch at a glance.

- Connection state, battery, and PebbleOS version
- About This Watch: model, finish, serial number, hardware, and firmware
- More than one watch: add watches and switch between them; one is connected at a time, and each
  keeps its own apps

**Profiles**: each person sharing a device has a profile with their own watches, notification
settings, calendars, weather places, and health history. A replacement watch carries on the
same history.

**PebbleOS**: installs PebbleOS on a new watch while you add it, and offers updates when a new
release is out.

**Apps**: watchfaces and apps from the Pebble and Rebble app stores, in one catalog, per watch. Apps
with a phone side (PebbleKit JS) run it here, settings pages included; network access is off
for each until you allow it.

**Notifications**: your device's notifications on the watch, with Dismiss and Mute from the
watch. Each app is On, Off, or Always: during the device's Do Not Disturb or silent mode,
notifications arrive quietly (no vibration or light) unless the app is set to Always.
Notifications that arrive while the watch is away are sent when it reconnects, if it does within
10 minutes and they are still open; only the newest vibrates. On watches that show pictures
(Pebble Time 2 and Round 2), a notification's picture can be shown, once turned on in
Preferences.

**Calls**: incoming calls ring on the watch with the caller's name from your contacts, and the
watch can answer or decline them.

**Calendar**: events from the calendars you pick go on the watch's timeline, two days back and a
week ahead, with their alarms as reminders.

**Weather**: the watch's Weather app, for your current location and places you add, from
Open-Meteo (default), MET Norway, the US National Weather Service, or Core Devices' service.

**Music**: the watch shows what is playing and controls the player; its volume buttons change
the device's volume.

**Health**: the day's steps, distance, active time, and calories; last night's sleep; heart rate
on watches with a sensor; steps and sleep by week, month, or year (monthly averages); and recent
walks, runs, and workouts. Each day opens hour by hour: steps, the night's light and deep sleep,
heart rate, and blood oxygen when the watch measured it. The
history stays on the device, kept per profile, for as long as you like. Health Settings switch
tracking and heart rate monitoring and delete old history; the profile holds height, weight,
and age.

**Dictation**: watch apps that take speech, such as replies and Home Assistant, are transcribed
on this device, in English. Turned on in Preferences, which downloads the speech model (135 MB).

**Preferences**: units (the system's, metric, or imperial), which profile opens at launch,
dictation, pictures in notifications, and which stores the Catalog shows.

**In the background**

- Keeps running with the window closed, as a separate service without the window's memory,
  starts at login if you like, and reconnects on its own
- Sets the watch's clock and time zone, and keeps them in step

**Advanced** (main menu): the connection's details and the running app, the watch's
capabilities, ping, restart, saving the watch's log and core dump for bug reports, firmware and
app installs from files, the latest recovery firmware, and restart into recovery firmware.

## Getting started

1. Install Lithidion (below).
2. Keep the watch nearby with Bluetooth on, and press **Add a Watch**.
3. Pick your watch from the list and press **Pair**, then confirm the request on the watch.

**Only one device should be paired with a watch at a time.** A watch paired again with another
device loses this one's pairing; Lithidion notices and pairs again on its next connection.

## Install

### Flatpak (recommended)

| <a href="https://ajdunevent.github.io/AJHub/"><picture><source media="(prefers-color-scheme: dark)" srcset="https://ajdunevent.github.io/AJHub/badges/button-dark.svg"><img src="https://ajdunevent.github.io/AJHub/badges/button-light.svg" alt="AJHub, published with FrogPak" height="48"></picture></a> | <a href="https://furihub.github.io/repo/"><picture><source media="(prefers-color-scheme: dark)" srcset="https://furihub.github.io/repo/badges/button-dark.svg"><img src="https://furihub.github.io/repo/badges/button-light.svg" alt="FuriHub, published with FrogPak" height="48"></picture></a> |
| --- | --- |
| x86_64 & aarch64 | aarch64 only |

## Privacy

Everything stays on your device, in `~/.local/share/lithidion/` (or
`~/.var/app/net.dunevent.Lithidion/data/lithidion/` under Flatpak). Notifications are read on
this device and sent only to your watch; their history stays on it for a week. Calendar
events go only to the watch, and contacts are read only to put a name to a caller. Health data
comes from the watch and stays on the device, and so does dictation: speech is transcribed here
and the text goes only to the watch. Lithidion goes online for four things:

- **PebbleOS**: when a watch connects, it looks at the latest release on
  [GitHub](https://github.com/coredevices/PebbleOS/releases), and downloads one only when you
  install it.
- **Weather**: the forecast for your places comes from the service you pick, which receives
  their coordinates. Place search uses Open-Meteo. Your current location is used only if you
  allow it, through the system's location permission.
- **The app stores**: browsing, searching, and installing ask the
  [Pebble](https://apps.repebble.com) and [Rebble](https://apps.rebble.io) stores, without an
  account. Search goes through Algolia, the stores' search service. Installed store apps are
  checked for newer versions when the Apps tab shows them.
- **Dictation**: turning it on downloads the speech model once from
  [Hugging Face](https://huggingface.co/handy-computer/parakeet-tdt_ctc-110m-gguf).

## Support the project

If Lithidion is useful to you, you can [support its development](https://dunevent.net/donate).

Bugs and ideas: [issues](https://dev.dunevent.net/aj/Lithidion/issues). Building, testing, and the
protocol notes: `docs/DEVELOPMENT.md`.

## Credits

Lithidion implements the Pebble protocol from the documented behaviour of the official
[Pebble app](https://github.com/coredevices/mobileapp) and
[PebbleOS](https://github.com/coredevices/PebbleOS), both open source from Core Devices, and
copies none of their code.

Dictation uses [transcribe.cpp](https://github.com/handy-computer/transcribe.cpp) (MIT) with
NVIDIA's Parakeet TDT-CTC 110M, as converted by handy-computer
([CC BY 4.0](https://creativecommons.org/licenses/by/4.0/)), and
[oxideav-speex](https://github.com/OxideAV/oxideav-speex) (MIT) for the watch's audio.

Not affiliated with Core Devices or Pebble.

## AI Disclaimer

This project has relied extensively on AI-assisted development, primarily Anthropic's Claude (various models), under human direction and manual testing.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE).
