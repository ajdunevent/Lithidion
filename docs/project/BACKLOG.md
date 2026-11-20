# Lithidion backlog

Items deferred on purpose, with what they wait for. Updated 2026-11-16.

## Before the first release

- **Branding**: colours and screenshots in the metainfo, to go with the app icon.
- **Hardware check of 0.1.0**: pairing from scratch, reversed and forward PPoGATT, reconnection
  after the watch leaves range, a bond the watch forgot, switching watches, start at login.

## Planned, in order

1. ~~**Apps and Store**~~: done in 0.1.0. See Apps below for what is left.
2. ~~**Notifications**~~ and calls: done in 0.1.0. See Notifications below.
3. ~~**Calendar, weather, music, and time**~~: done in 0.1.0. See Calendar, weather, and music
   below.
4. ~~**Health**~~: done in 0.1.0. See Health below.
5. ~~**Advanced**~~: log dumps and core dumps, saved to files from Advanced. (Firmware updates
   moved ahead of the plan; see Firmware below.)
6. ~~**PebbleKit JS**~~ and configuration pages: done in 0.1.0. See PebbleKit JS below.

## Watch tab

- On the Watch leaves out Album Art (Lithidion sends no album art yet), the menu highlight colours
  (the official app holds them back: some values crash the watch), language, the background
  worker, and the debug settings (motion sensitivity, ambient light threshold).
- On the Watch shows no sign that a change is still waiting for the watch.

## Transport

- The forward PPoGATT service is registered on first need and kept, as the official app does.
  If a bonded forward-mode watch reconnects before Lithidion has registered it after a start,
  registering at startup for watches last seen in forward mode is the next step.
- Reversed PPoGATT V1 (Pebble 2, `30000003-…`) is not offered; the official app keeps it off by
  default too.
- PPoGATT ack timeout (5 s) and resend count (3) sit between the official app's and PebbleOS's
  harness values; revisit with hardware logs.

## Firmware

- PutBytes resume after an interrupted transfer (init with the resume magic) is not used; an
  interrupted install starts again from the beginning.
- The update check runs at every connection, using a release seen in the last six hours. A
  preference to turn it off can follow if wanted.
- Recovery firmware is offered only from Advanced and is not checked for updates.
- Bundles stay in the cache folder (`~/.cache/lithidion/firmware/`); old ones are not yet
  cleaned up.

## Apps

- Watches used with another phone are taken over silently (databases cleared, Lithidion's list
  sent), as the official app does, instead of asking.
- The watch's own apps (Settings, Music, Health…) are not listed or reordered.
- Reordering is Move Up and Move Down; drag and drop can follow.
- Update checks ask the store for each installed app once per run; the Pebble store's bulk
  endpoint would do it in one request.
- Hearts, the cloud locker, and timeline tokens need an account and are left out.
- App permissions (the `app_permissions_support` word) are not sent.

## Notifications

- Quick replies and an app's own notification actions need app-specific support; only Dismiss
  and Mute are offered.
- Notifications missed while the watch was away are kept in memory only, so they are lost if
  Lithidion quits before it reconnects.
- Pictures are kept for the 32 most recent notifications, in memory; an older one opened from
  the watch's history shows an empty space where its picture was.
- The phone's ringer cannot be silenced from the watch.
- Icons and colours are picked from the category hint and app name; per-app choices could follow.
- GNOME's Do Not Disturb is read through the settings portal; if Phosh's portal does not offer
  it, reading dconf directly needs `--filesystem=xdg-config/dconf:ro`.

## Health

- Export (CSV or similar) of the kept history.
- Notification app icons: an embedded image (`image-data`) is not used as the icon, as it is
  often the sender's picture rather than the app's; it is the notification's picture instead.
- Heart rate log records (data logging tag 85) are not kept; the minutes carry heart rate.
- SpO2 is shown on a day's page only; there is no trend of it.
- Resting and elevated heart rates are sent as the watch's defaults (70, 100), not measured.
- Typicals, averages, and the month and year charts are recomputed from the stored minutes each
  time; a cache can follow if it grows slow on years of history.
- A year's bar does not open its month.

## Profiles

- Profiles have no passwords: anyone with the device can switch.
- A watch belongs to one profile; moving it means removing it and pairing it again in the other.
- Units are a device preference, not per profile.

## Calendar, weather, and music

- Weather services that need the user's own API key (OpenWeatherMap, WeatherAPI.com, Pirate
  Weather): a key field per service.
- Weather pins on the timeline (sunrise, sunset, the day's forecast) and weather v4 minors 1 to
  5 (more detail for the watch's Weather app).
- The current location is shown as "Current Location" unless the location portal names it; a
  reverse lookup would need another service.
- Calendar changes reach the watch within 15 minutes; following EDS's live views would make it
  immediate.
- RSVP actions (Accept, Maybe, Decline) on calendar pins.
- Album art: the watch's requests are refused. MPRIS gives only the art's address, and under
  Flatpak the files players point to are mostly out of reach.
- The player's own volume for players that offer it.

## Dictation

- English only: the model is Parakeet TDT-CTC 110M. Other languages would need a choice of model
  (Parakeet TDT 0.6B v3 covers 25 languages at 740 MB); `tools/voiceprobe` measures candidates.
- Command and NLP sessions (Pebble's old voice commands and reminders) and audio streams for apps'
  companions are refused.
- Voice replies to notifications: unlikely to be feasible. GNOME Shell and Phosh do not offer
  inline replies, so apps attach no reply action; replying to SMS through ModemManager directly
  would bypass Chatty's history.
- The engine needs a fixed CPU baseline in every build (`engine_cmake_args` in `dev.sh`, and the
  manifest's build-options). The aarch64 one, ARMv8.2 with dot product, leaves out Cortex-A53
  phones (PinePhone, Librem 5); one build for all would need ggml's per-CPU variants, loaded at
  run time.

## PebbleKit JS

- Tokens use the app's UUID; the official app uses the store's developer id when it has one, so
  servers that key settings by watch token see Lithidion as a different phone.
