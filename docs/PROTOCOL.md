# Pebble Protocol notes

Lithidion's working notes on how a phone talks to a Pebble watch: the Bluetooth LE transport,
PPoGATT, Pebble Protocol framing, and the messages Lithidion uses. Everything is stated in our own
words and implemented from scratch; no upstream code is copied.

## Conventions

**Confidence**

- **Confirmed**: done by the official app in production, or seen on hardware by us.
- **Probable**: consistent with the sources but not yet seen on hardware by Lithidion.
- **Unverified**: inferred, or seen once.

**Sources**

| Tag | Source | Revision |
|---|---|---|
| LP | [coredevices/mobileapp](https://github.com/coredevices/mobileapp), `libpebble3` (GPL-3.0) | `6361b33` (2026-11-08) |
| OS | [coredevices/PebbleOS](https://github.com/coredevices/PebbleOS), including its phone-side test harness in `tests/integration/harness` (Apache-2.0) | `f4926c9` (2026-11-13) |
| OLD | The first Lithidion, tested on hardware in 2026 | |

Paths below are relative to `libpebble3/src/commonMain/kotlin/io/rebble/libpebblecommon/` for LP
and to the repository root for OS.

**Notation**: bytes in hex; multi-byte integers big-endian unless stated; offsets zero-based.

## 1. Discovery

- Watches advertise manufacturer data under company ID `0x0154` (Pebble) or `0x0EEA` (Core
  Devices). LP `connection/Scanning.kt`. Confirmed for `0x0154`, Probable for `0x0EEA` (byte
  order read from a little-endian helper).
- Manufacturer data, little-endian: payload type `u8`, serial (12 ASCII bytes), then, from
  firmware 3.0: hardware platform `u8`, model (color) `u8`, firmware major, minor, patch (`u8`
  each), flags `u8` (bit 0 running recovery firmware, bit 1 first use). LP
  `connection/bt/ble/pebble/PebbleLeScanRecord.kt`. Probable.
- A watch waiting for a new phone advertises the pairing service `0000fed9-…`; reconnection
  advertising leaves it out. OS `tests/integration/harness/ble/__init__.py`. Probable.
- emery, flint, and gabbro watches do not advertise while disconnected from a bonded phone; the
  phone reconnects to the known address. LP `PebbleBle.kt` (`advertisesWhenNotConnected`).
  Probable.

## 2. GATT layout

All Pebble UUIDs end in `-328E-0FBB-C642-1AA6699BDADA`. LP `pebble/LEConstants.kt`, confirmed.

| UUID | Host | Purpose |
|---|---|---|
| `0000fed9-0000-1000-8000-00805f9b34fb` | watch | Pairing service |
| `00000001-…` | watch | Connectivity status (read, notify) |
| `00000002-…` | watch | Pairing trigger (write) |
| `00000005-…` | watch | Connection parameters (notify, write) |
| `40000000-…` | watch | PPoGATT, reversed V2 |
| `40000001-…` | watch | V2 data from watch (notify) |
| `40000003-…` | watch | V2 data to watch (write without response) |
| `30000003-…` | watch | PPoGATT, reversed V1 (Pebble 2) |
| `30000004-…` | watch | V1 data, both directions |
| `10000000-…` | phone | PPoGATT, forward |
| `10000001-…` | phone | Forward data (watch writes without response; phone notifies) |
| `10000002-…` | phone | Forward meta (read) |
| `0000180f-…` / `00002a19-…` | watch | Standard Battery service and level |

- Forward data characteristic: write-without-response and notify, writes need encryption; meta:
  read, needs encryption. LP Android `GattServer.android.kt`. Confirmed.
- Meta value: `00 01` then 16 zero bytes then `01` (19 bytes). LP `PebbleBle.android.kt`;
  OLD. Confirmed. The OS harness ends it with `00` instead.

## 3. Connecting (LP `PebbleBle.kt`, `PebblePairing.kt`, `ConnectivityWatcher.kt`)

1. Connect and discover services.
2. Pick the transport: reversed V2 when the watch has `40000000-…`; otherwise forward, with the
   phone's GATT service registered before the watch looks for it. Reversed V1 only when enabled
   (off by default). Confirmed.
3. Subscribe to connection parameters and write `00 01` (with response). Older firmware lacks
   it. Confirmed.
4. Request ATT MTU 339. Confirmed.
5. Read and subscribe to connectivity. Value: flags byte, two reserved bytes, pairing error code.
   Flags: bit 0 connected, 1 paired, 2 encrypted, 3 has bonded gateway, 4 supports pinning
   without slave security, 5 remote attempted stale pairing. Fewer than 4 bytes means a wedged
   watch. Confirmed.
6. Pair when the watch says unpaired, the phone has no bond, or the link does not encrypt
   within 5 s of a paired report. Confirmed.
7. Reversed: subscribe to the watch's data characteristic after pairing, then start PPoGATT.

**Pairing trigger** (aplite, diorite, emery, flint, gabbro; basalt and chalk do not use it):
write one byte, with response: bit 0 pin address, bit 1 no security request, bit 2 force security
request (= not bit 1), bit 3 auto-accept future pairing, bit 4 watch as GATT server (reversed V1).
The official app pins the address and sets "no security request" when connectivity bit 4 is set,
and then asks the OS to bond itself; otherwise the watch requests security. Waiting for the bond
allows 60 s, since it needs a confirmation on the watch. Confirmed. A write error on the trigger
is ignored by the official app. Confirmed.

The official iOS app instead neither pins nor bonds itself: it writes `04` (force security
request) and the watch starts security (LP `di/LibPebbleModule.ios.kt`, `pinAddress = false`,
`phoneRequestsPairing = false`). Lithidion pairs as Android does and falls back to this when a
phone-started pairing does not finish. Seen on a Pebble Time 2 in recovery firmware 4.47.0: SMP
pairing completed, both DHKey checks included, but the watch never answered the encryption that
follows (`LE Start Encryption` accepted, no `Encryption Change`), until BlueZ disconnected. The
watch-started path stalled the same way. The same watch paired at once with a phone, so it is
specific to that computer's Bluetooth (an Intel controller, kernel 7.3.3) with that recovery
firmware, not every Time 2 on it.

**One phone at a time** (PebbleOS `services/bluetooth/pairability.c`, `settings/bluetooth.c`): the
watch advertises, and so can be paired, only while it has no gateway bonding. Once paired it stops
advertising until that pairing is removed in its Settings → Bluetooth or the watch is reset, so a
watch found while scanning needs no reset first. Nothing over the protocol removes the watch's
pairing; BLE control `SetDiscoverablePairable` (only over a connection) makes it discoverable for a
time. The official app's Remove leaves the watch's pairing in place. Confirmed.

- BlueZ: the agent needs an explicit `RequestConfirmation` handler that accepts; without one
  pairing fails with "Numeric comparison failed". OLD, confirmed.
- BlueZ: `Device.Connect()` may never return although the link comes up; poll `Connected`.
  `Pair()` may also not return; poll `Paired`. OLD and Ringmaster on FLX1s, confirmed.
- BlueZ: removing a bond fails while connected; disconnect first, then rescan for the address
  before connecting again. OLD, confirmed.
- BlueZ on FuriOS: `SetDiscoveryFilter` may fail with "Operation already in progress"; the scan
  still works. OLD, confirmed.
- BlueZ on an x86_64 desktop: connecting to an unpaired watch can go to classic Bluetooth (BR/EDR)
  and hang paging it, when the watch's advertising flags do not say "BR/EDR not supported"
  (flags bit 2). Connecting while an LE-only scan runs, right after a live advertisement, worked.
  OLD, confirmed; Lithidion 0.1.0 hit the hang on the desktop with a factory-reset watch.
- bluer's scan reports every device BlueZ has cached once when it starts; only a later report for
  the address means a live advertisement.
- Forward: the watch subscribes to the phone's data characteristic more than once in a
  connection; queued data must move to the newest subscription. OLD, confirmed.

## 4. PPoGATT (LP `ppog/PPoG.kt`, `ppog/PPoGPacket.kt`; OS `harness/ble/ppogatt.py`)

- Every packet starts with one byte: sequence number `<< 3 | type`. Types: 0 data, 1 ack,
  2 reset request, 3 reset complete. Sequence numbers run 0 to 31 and wrap. Confirmed.
- Reset request: header, version byte (0 or 1). OS's harness appends the 12-byte serial; the
  official app sends only the version. Reset complete (version 1): header, receive window,
  transmit window; version 0 sends no windows and implies 4. Confirmed.
- Opening: the side that starts sends a reset request; the other answers with reset complete;
  the starter confirms with its own reset complete. Forward: the watch starts. Reversed: the
  phone starts, and if the watch starts its own reset meanwhile, the phone answers that instead.
  Confirmed.
- Windows: the phone offers 25 and 25. Its transmit window is the smaller of 25 and the
  receive window in the watch's reset complete; its receive window the smaller of 25 and the
  watch's transmit window. Confirmed.
- Data payload per packet: MTU minus 4 (ATT header 3, PPoGATT header 1); 19 bytes at the default
  MTU of 23. Confirmed.
- Receiving data: the expected sequence number is delivered and acknowledged with its own
  number; anything else gets the last acknowledgement again. Confirmed.
- Acks are cumulative: an ack for `n` releases every in-flight packet up to and including `n`.
  A repeated identical ack means the watch lost packets: resend the in-flight ones. Confirmed.
- Timeouts: the official app resends in-flight packets after 10 s without an ack and gives up
  after 2 resends; OS's harness uses 3 s and 3. Lithidion uses 5 s and 3. Unverified.
- A reset request or reset complete from the watch inside an open session ends that session;
  the official app disconnects and reconnects. Confirmed.

## 5. Pebble Protocol framing

- Frame: length `u16` (payload only), endpoint `u16`, payload. A PPoGATT data packet carries any
  slice of the byte stream, so frames are reassembled from the stream. LP
  `connection/PebbleProtocolRunner.kt`; OS `harness/ble/transport.py`. Confirmed.
- Integers inside payloads are big-endian unless a field says otherwise. Strings: "pstring"
  is a `u8` length then UTF-8; fixed strings are padded with `00`, and `FF` also ends one
  (erased flash). LP `structmapper/types.kt`. Confirmed.
- An optional trailing field has no presence byte: it is present when bytes remain. Confirmed.

| Endpoint | Value |
|---|---|
| Time | 11 |
| Watch version | 16 |
| Phone version | 17 |
| System message | 18 |
| Music control | 32 |
| Phone control | 33 |
| App message | 48 |
| App run state | 52 |
| Imaging | 53 |
| Ping | 2001 |
| Log dump | 2002 |
| Reset | 2003 |
| Factory registry | 5001 |
| App fetch | 6001 |
| Data logging | 6778 |
| Get bytes | 9000 |
| Put bytes | 48879 |
| BlobDB v1 | 45531 |
| BlobDB v2 | 45787 |
| Timeline actions | 11440 |
| Health sync | 911 |

LP `protocolhelpers/ProtocolEndpoint.kt`. Confirmed.

## 6. Session start (LP `connection/Negotiator.kt`, `connection/TransportConnector.kt`)

All within 20 s, after PPoGATT opens:

1. Wait for the watch's phone version request (endpoint 17, `00`).
2. Send the phone version response (below).
3. Request the watch version (endpoint 16, `00`) and the model (factory registry `mfg_color`).
4. Set the time (endpoint 11, set UTC).
5. Unless the watch runs recovery firmware, request the running app (endpoint 52, `03`) and
   wait for the first start message.

Confirmed. Requests routed to the watch's main task can take long right after a boot; the
first Lithidion needed 30 s for some. OLD, confirmed. Recovery firmware 4.9.142 answered the
model request after 12 s, and sends 3-byte messages on endpoint 0 (recovery) that Lithidion does
not interpret yet. Lithidion, observed once.

### Phone version response (endpoint 17)

`01`, protocol version `u32` (`FFFFFFFF`), session capabilities `u32` (0), platform flags `u32`
(low 4 bits: OS type, 2 = Android, 4 = Linux), response version `u8` (2), app major, minor,
bugfix `u8` (4, 4, 2), protocol capabilities (8 bytes, bit `n` of the 64 is capability `n`).
LP `packets/System.kt`, `services/SystemService.kt`. Confirmed.

- The music endpoint only switches on for OS type Android; the watch ignores every other value.
  OS firmware `music/endpoint.c`, cited by OLD. Confirmed. Lithidion reports Android.
- Lithidion advertises only the capabilities it implements (section 7).

### Watch version response (endpoint 16)

Offsets from the start of the payload:

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | `01` |
| 1 | 47 | Running firmware: timestamp `u32`, version tag (32, fixed string), git hash (8), flags `u8`, hardware platform `u8`, metadata version `u8` |
| 48 | 47 | Recovery firmware, same layout |
| 95 | 4 | Bootloader timestamp |
| 99 | 9 | Board |
| 108 | 12 | Serial |
| 120 | 6 | Bluetooth address |
| 126 | 4 | Resource CRC |
| 130 | 4 | Resource timestamp |
| 134 | 6 | Language |
| 140 | 2 | Language version |
| 142 | 8 | Capabilities |
| 150 | 1 | Unfaithful, optional |
| 151 | 2 | Health insights version, optional |
| 153 | 2 | JavaScript version, optional |

Firmware flags: bit 0 recovery firmware, 1 BLE firmware, 2 dual slot, 3 slot 0. Version tags look
like `v4.35.0` or `v4.35.0-rc1`. "Unfaithful" means the watch has been connected to another phone
since its last BlobDB clear. LP `packets/System.kt`; offset 150 OLD, confirmed.

### Model (endpoint 5001)

Request: `00`, pstring key `mfg_color`. Response: `01`, length `u8`, value; the model number is
the value read as `i32`. Error: `FF`. LP. Confirmed.

### Time (endpoint 11)

Set UTC: `03`, Unix time `u32`, UTC offset in minutes `i16`, pstring time zone name. The official
app rounds to the nearest second and resends hourly. The watch may ask with `04`; the answer is a
new set UTC. LP `services/SystemService.kt`. Confirmed.

PebbleOS looks the name up in its own zone database and, when it finds it, uses that zone and
ignores the offset; only an unknown name falls back to the offset (OS `services/clock/service.c`).
A name that disagrees with the offset therefore sets the wrong time, so Lithidion sends the name
only when its offset matches the clock's, and an empty name otherwise. The zone comes from
timedated (`org.freedesktop.timedate1`, `Timezone`), followed through `PropertiesChanged`; a
change resends the time and the calendar at once. Confirmed.

### App run state (endpoint 52)

`01` + UUID (16): started; `02` + UUID: stopped; `03`: request. LP `packets/AppRunState.kt`.
Confirmed.

### Ping and reset

- Ping (2001): `00` + cookie `u32`; pong `01` + cookie. Confirmed.
- Reset (2003): `00` reboot, `01` core dump, `FE` factory reset, `FF` reboot into recovery
  firmware. Confirmed.

## 7. Capabilities

Bit numbers of the 64-bit capability field, used in both directions. LP `packets/System.kt`.

| Bit | Capability | Lithidion advertises |
|---|---|---|
| 0 | App run state | yes |
| 1 | Infinite log dump | yes |
| 2 | Extended music | yes |
| 3 | Two-way dismissal | |
| 4 | Localization | |
| 5 | 8 kB app messages | |
| 6 | Health insights | |
| 7 | App dictation | yes |
| 8 | Send text app | |
| 9 | Notification filtering | |
| 10 | Unread core dump (sent by the watch; not read from the phone) | |
| 11 | Weather app | yes |
| 12 | Reminders app | |
| 13 | Workout app | |
| 14 | Smooth firmware install progress | |
| 15 | Custom vibe patterns | |
| 16 | JavaScript bytecode version appended | |
| 17 | Image fetch (PebbleOS: imaging) | yes |
| 18 | Notification images (sent by the watch) | |
| 21 | Firmware update across disconnection | |
| 22 | BlobDB version | |
| 23 | Settings sync | |
| 24 | Weather DB v4 | |
| 25 | Unknown timeline attributes | |
| 26 | Notification window state | |
| 27 | App permissions | |
| 28 | Multiple language packs | |

## 8. Hardware

Hardware platform numbers (watch version and scan record), with the app platform. LP
`metadata/WatchHardwarePlatform.kt`, `metadata/WatchType.kt`. Confirmed.

| Number | Revision | App platform |
|---|---|---|
| 1–6 | ev1, ev2, ev2_3, ev2_4, v1_5, v2_0 | aplite |
| 7, 8, 10 | snowy_evt2, snowy_dvt, snowy_s3 | basalt |
| 9, 11 | spalding_evt, spalding | chalk |
| 12, 14 | silk_evt, silk | diorite |
| 13 | robert_evt | emery |
| 15 | asterix | flint |
| 16, 17, 18 | obelix_evt, obelix_dvt, obelix_pvt | emery |
| 19, 20, 21 | getafix_evt, getafix_dvt, getafix_dvt2 | gabbro |
| 242–255 | big boards and emulators | by board |

Apps built for one platform run on others: emery runs emery, basalt, diorite, and aplite apps;
flint runs flint, diorite, and aplite; gabbro runs gabbro and chalk; basalt runs basalt and
aplite; diorite runs diorite and aplite. Confirmed.

Model numbers (factory registry `mfg_color`): 1–10 Pebble and Pebble Steel; 11–16 Pebble Time
and Time Steel; 17–23 Pebble Time Round; 24–30 Pebble 2; 31–33 and 36–39 Pebble Time 2; 34–35
Pebble 2 Duo; 40–43 Pebble Round 2. LP `metadata/WatchColor.kt`. Confirmed.

## 9. Firmware (LP `connection/endpointmanager/FirmwareUpdater.kt`, `services/PutBytesService.kt`, `disk/pbz/DiskUtil.kt`; OS `pebble/services/put_bytes/`, `pebble/services/firmware_update/`)

**Bundles.** A `.pbz` is a zip. Either one `manifest.json` sits at its root, or one each sits
under `slot0/` and `slot1/` for watches that keep two PebbleOS slots. A manifest names the
firmware (`name`, `type` of `normal` or `recovery`, `hwrev`, `size`, `crc`, `versionTag`, and
`slot` in slot folders) and, optionally, the system resources (`name`, `size`, `crc`).
Confirmed.

**CRC.** STM32 hardware CRC: the data read as little-endian 32-bit words, polynomial
`04C11DB7`, initial value `FFFFFFFF`, no reflection, no final XOR. A final partial word is
padded with zeros at its high end. Check value: the word `12345678` gives `DF8A8A2B`. Confirmed
against the manifests of the v4.47.0 obelix_pvt bundle.

**Safety checks** before anything is sent, as the official app makes them: the type is
`normal` or `recovery`; size and CRC are non-zero; `hwrev` is the watch's hardware revision
(section 8); for a normal build on a dual-slot watch, `slot` is the slot being written; and the
bytes match the manifest's sizes and CRCs (our addition). Confirmed.

**Which build.** Recovery firmware bundles install directly. A watch in recovery firmware takes
the `slot0` build (or the only build). A watch running PebbleOS from slot N takes the build for
the other slot, if the bootloader would boot it afterwards: a development build always, a
release only if its version is at least the running one. Otherwise the watch is restarted into
recovery firmware (Reset endpoint 2003, payload `FF`), and the `slot0` build is installed from
there. Probable; installing from recovery firmware is confirmed by OLD.

**Sources.** GitHub releases of coredevices/PebbleOS. Assets are named
`normal_<hwrev>_<tag>.pbz` (both slots), `normal_<hwrev>_<tag>_slot0.pbz` and `_slot1.pbz`, and
`recovery_<hwrev>_<tag>.pbz`, under
`https://github.com/coredevices/PebbleOS/releases/download/<tag>/<asset>`. The official app
asks Core's own server instead (`dash.repebble.com/api/ota/latest`); Lithidion does not.
Confirmed.

**Sequence.**

1. System message (endpoint 18) `00 01`, then the bytes already sent (u32, little-endian), and
   the bytes to send (u32, little-endian): firmware plus resources. The watch answers `00 0A`
   and a status: `00` stopped, `01` started, `02` cancelled. Anything but started ends the
   install. Confirmed.
2. The firmware, then the resources, each over PutBytes (endpoint 48879):
   - Init: `01`, size (u32), object type (u8: `01` firmware, `02` recovery, `03` system
     resources), bank `00`. The watch erases flash first; the official app waits 35 s.
   - Put: `02`, cookie (u32), length (u32), at most 2000 bytes.
   - Commit: `03`, cookie, CRC (u32).

   Each is answered by `01` (ack) or `02` (nack) and the cookie; init's answer carries the new
   cookie. The official app waits 20 s for the rest. Abort is `04` and the cookie. Confirmed.
3. Install (`05` and the cookie) for the firmware, then for the resources, only after both have
   been committed. Confirmed.
4. System message `00 02` (update complete). The watch restarts into the new firmware.
   `00 03` reports a failed update. Confirmed.

PutBytes also resumes an interrupted transfer (init with magic `BE4354EF` and an offset);
Lithidion does not yet. Probable.

## 10. Apps (LP `connection/endpointmanager/AppFetchProvider.kt`, `connection/endpointmanager/blobdb/BlobDB.kt`, `connection/endpointmanager/AppOrderManager.kt`, `disk/pbw/`, `packets/blobdb/App.kt`; OS `docs/reference/blob-db.md`, `pebble/services/app_fetch_endpoint/service.c`)

**Bundles.** A `.pbw` is a zip with `appinfo.json` at its root (`uuid`, `shortName`,
`longName`, `companyName`, `versionLabel`, `watchapp.watchface`, `watchapp.hiddenApp`) and a
folder per platform (`basalt/`, `emery/`, …) holding `manifest.json` and the files it names:
`application`, and optionally `resources` and `worker`, each with `name`, `size`, and `crc`
(section 9's CRC). Old bundles keep their aplite build at the root. A watch takes the first
platform in its compatible list (section 8) that the bundle has; `targetPlatforms` in
`appinfo.json` can be wrong and is not used. Confirmed.

**Binary header.** An app binary starts with `PBLAPP\0\0`, then little-endian fields: header
version (2 bytes), SDK version major and minor (offsets 10, 11), app version major and minor
(12, 13), size (u16), entry offset (u32), CRC (u32), name (32 bytes, offset 24), company (32,
offset 56), icon resource id (u32, offset 88), symbol table (u32), process flags (u32, offset 96),
relocation count (u32), UUID (16, offset 104). Confirmed on a store bundle from November 2026.

**BlobDB v1** (endpoint 45531). Commands are a byte, a little-endian u16 token, the database id,
then: insert `01`, key (u8 length + bytes), value (u16 LE length + bytes); delete `04`, key;
clear `05`. The watch answers token and status: `01` success, `02` failure, `03` invalid
operation, `04` invalid database, `05` invalid data, `06` no such key, `07` full, `08` stale,
`09` not supported, `0A` locked, `0B` try later. Database ids: 1 pins, 2 apps, 3 reminders,
4 notifications, 5 weather, 6 canned responses, 7 health parameters, 8 contacts, 9 app configs,
10 health stats, 11 app glances, 12 settings. Confirmed.

**App database entry** (database 2, key the 16-byte UUID, 126 bytes): UUID, process flags (u32
LE), and icon id (u32 LE) from the binary header, app version (two bytes, from `versionLabel`),
SDK version (two bytes), two zero bytes, the name (96 bytes, NUL-padded). Watches with the
`app_permissions_support` capability accept a u32 LE permissions word after it. Confirmed.

**Watches used elsewhere.** The watch marks itself unfaithful when it bonds with a new phone or
changes gateway; a successful clear makes it faithful again. On a first connection, and
whenever the watch reports itself unfaithful, the official app clears every database but
settings (12) and sends its own data again. Lithidion does the same. Confirmed.

**App order** (endpoint 43981): `01`, a count byte, and the UUIDs, apps then faces; the watch
answers one status byte, `01` meaning done. Apps not listed keep their place after these.
Confirmed.

**Opening an app**: app run state (section 6) start, `01` and the UUID.

**App fetch** (endpoint 6001). An app in the database but not on the watch is fetched when it
is opened: the watch sends `01`, the UUID, and an install id (u32 LE). The phone answers `01` and
a result (`01` starting, `02` busy, `03` unknown UUID, `04` no data), then sends each part over
PutBytes with an app init: `01`, size, the object type with its top bit set (`85` binary,
`84` resources, `87` worker), and the install id (u32 BE). Each part is put, committed, and
installed (section 9). The watch waits 15 s for each init. Confirmed.

## 11. App stores (mobileapp `pebble/src/commonMain/kotlin/coredevices/pebble/services/AppstoreService.kt`, `AppstoreSources.kt`)

Both stores answer without an account. Bases: `https://appstore-api.repebble.com/api` (Pebble
App Store) and `https://appstore-api.rebble.io/api` (Rebble). Every request carries
`platform=android`, `hardware=<app platform>` and `filter_hardware=true`. Confirmed.

- `GET /v1/home/{watchfaces|apps}`: `applications`, plus `collections` and `categories` with
  their `application_ids` and a `links.apps` path for the full list.
- `GET <links.apps>&offset&limit`: `data`, and `links.nextPage` while there is more.
- `GET /v1/apps/id/{id}`: `data`, a one-app list. An app has `id`, `uuid`, `type`
  (`watchface`, `watchapp`; others carry nothing for the watch), `title`, `author`,
  `description`, `latest_release` (`version`, `pbw_file`, `release_notes`), `changelog`,
  `screenshot_images`, `list_image`, `icon_image`, `compatibility` (per platform `supported`),
  `hearts`, `website`, `source`.
- `POST /v1/apps/bulk` with `{"ids": [...]}`: several apps; Pebble store only.
- Search: Algolia, `POST https://<app id>-dsn.algolia.net/1/indexes/<index>/query` with the
  `X-Algolia-Application-Id` and `X-Algolia-API-Key` headers, `query`, `hitsPerPage`, and
  `tagFilters` `[type, "android"]`. Pebble: `GM3S9TRYO4`, index `apps`; Rebble: `7683OW76EQ`,
  index `rebble-appstore-production`; the search-only keys are those the official app ships.
  Hits have no `latest_release`: the app's own page gives the bundle link.

Lithidion shows both stores; an app in both is shown once, from the store with the newer
version, as the official app does.

## 12. Notifications (LP `packets/blobdb/Timeline.kt`, `notification/LibPebbleNotification.kt` (Android), `services/blobdb/TimelineService.kt`; OS `include/pbl/services/timeline/item.h`, `attribute.h`, `pebble/services/blob_db/notif_db.c`, `pebble/popups/notifications/notification_window.c`)

**Timeline item** (BlobDB 4 for notifications, key its 16-byte id; little-endian): id, parent id
(`ed429c16-f674-4220-95da-454f303f15e2` for phone notifications, as the official Android app),
timestamp (i32, seconds), duration (u16, minutes), type (`01` notification, `02` pin, `03`
reminder), flags (u8), status (u8), layout (`04` generic notification), payload length (u16),
attribute count, action count, then the attributes and actions. An attribute is an id, a u16
length, and the value; an action is an id, a type (`02` generic, `03` response, `04` dismiss), an
attribute count, and its attributes. Confirmed.

Attributes used: 1 title (up to 64 bytes), 2 subtitle (64), 3 body (512), 4 tiny icon (u32,
system icons ORed with `80000000`), 6 large icon, 28 background colour (one byte, two bits each
of alpha, red, green, blue), 30 app name. Icons: 1 generic notification, 2 missed call, 19 email,
45 SMS, 46 muted, 51 dismissed, 62 failed, 78 incoming call. Confirmed.

**Quiet delivery.** Flag bit 6 marks a notification silenced on the phone. When the watch's
`notifRespectPhoneSilence` setting is on, such a notification is stored without a popup,
vibration, or backlight. The setting is a settings-database entry (BlobDB 12, key the
NUL-terminated name, value one byte), written with insert-with-timestamp (`0D`: token, database,
u32 LE time, key, value) on watches with the `BlobDbVersion` capability. Confirmed in PebbleOS.

**Removing one.** The watch keeps only status changes for a notification it already has: an
insert of the same item with status bit 4 (dismissed) takes it off the watch. An insert carrying
status bits for an item it does not have is ignored. Confirmed in PebbleOS.

**Actions** (endpoint 11440). The watch sends `02`, the item id, the action id, and attributes;
the phone answers `11`, the item id, a result (`00` done, `01` failed), an attribute count, and
attributes for the result screen: a large icon and a subtitle ("Dismissed"). Confirmed.

**On the phone.** Notifications are read from the session bus with `BecomeMonitor`, matching
`Notify` calls, the notification server's method returns (which carry the new id; the server is
found with `GetNameOwner`) and `NotificationClosed` signals. `CloseNotification` dismisses one.
The phone is quiet when feedbackd's `org.sigxcpu.Feedback` `Profile` property is `silent`, or
GNOME's `org.gnome.desktop.notifications` `show-banners` is false (Phosh's Do Not Disturb), read
through the settings portal. Lithidion's own reading.

## 13. Calls (LP `packets/PhoneControl.kt`, `connection/endpointmanager/phonecontrol/PhoneControlManager.kt`)

Endpoint 33. A message is a command and a u32 BE cookie naming the call. Phone to watch: `04`
incoming (then the number and the name as length-prefixed strings, up to 31 bytes each), `06`
missed (same), `07` ring, `08` start (answered), `09` end. Watch to phone: `01` answer, `02`
hang up. Confirmed.

On the phone, calls come from ModemManager's voice interface on the system bus
(`org.freedesktop.ModemManager1.Modem.Voice` `CallAdded`, `org.freedesktop.ModemManager1.Call`
`StateChanged`, `Number`, `Direction`, `Accept`, `Hangup`). FuriOS provides it through ofono2mm,
whatever the phone app. States: 3 ringing in, 4 active, 6 waiting, 7 terminated. Lithidion's own
reading.

## 14. Weather (LP `database/entity/WeatherAppEntity.kt`, mobileapp `pebble/.../weather/WeatherFetcher.kt`; OS `include/pbl/services/blob_db/weather_db.h`, `weather_type_tuples.def`, `pebble/services/blob_db/weather_db.c`)

BlobDB 5, keyed by a UUID per place; little-endian. The watch keeps weather only when the phone
advertised the `WeatherApp` capability. Confirmed.

- **v3**: version `03`, current temperature (i16), current conditions (u8), today's high and
  low (i16 each), tomorrow's conditions, high and low, last update (u32 Unix time), current
  location (bool), then the strings: a u16 total length, and the place name and a short phrase
  each as a u16 length and bytes.
- **v4** (watches with `WeatherDbV4`): version `04`, the same 18 bytes, a minor version, then
  for minor 0: feels-like (i16, `7FFF` unknown), UV index × 10 (i16, -1 unknown), precipitation
  chance (i16, -1), wind speed (u16, 0), wind direction (u16, `FFFF`), latitude and longitude ×
  100 (i16, `8000` unknown), a day count and 7 days of high, low (i16) and conditions (u8,
  `FF` unknown), an hour count (0 or 24) and today's 24 conditions (u8) and temperatures (i8),
  then the strings. Later minors add fields before the strings; the watch finds the strings by
  the minor. Lithidion sends minor 0.
- **Conditions**: 0 partly cloudy, 1 cloudy, 2 light snow, 3 light rain, 4 heavy rain, 5 heavy
  snow, 6 generic, 7 sun, 8 rain and snow, 255 unknown.

Temperatures and wind follow the Units preference; System is °F and mph for US (and Liberia and
Myanmar) locales, else °C and km/h.

**Sources.** Open-Meteo (`api.open-meteo.com/v1/forecast`, WMO codes; `geocoding-api` for place
search), MET Norway (`api.met.no/weatherapi/locationforecast/2.0/compact`, symbol codes; asks
for an identifying User-Agent), the US National Weather Service (`api.weather.gov/points/…`,
then its forecast and hourly forecast; text forecasts), and Core Devices
(`weather-api.repebble.com/api/v1/geocode/{lat}/{lon}?units=e|m`, The Weather Company's icon
codes, which the official app maps). Lithidion's own reading.

## 15. Music (LP `packets/Music.kt`, `connection/endpointmanager/musiccontrol/MusicControlManager.kt`)

Endpoint 32. Watch to phone: `01` play/pause, `02` pause, `03` play, `04` next, `05` previous,
`06` volume up, `07` volume down, `08` get current track. Phone to watch: `10` track (artist,
album, title as length-prefixed strings, then the length in ms, track count, and track number,
u32 LE, each optional from the end), `11` play state (state u8: 0 paused, 1 playing, 4
unknown; position ms and rate %, u32 LE; shuffle and repeat, 0 unknown; a byte, 1 when next and
previous seek within the track), `12` volume (percent), `13` player (id and name). The watch
uses the extended messages when the phone advertises `ExtendedMusic`. Confirmed.

On the phone: MPRIS players on the session bus (`org.mpris.MediaPlayer2.*`,
`PlaybackStatus`, `Metadata`, `Position`, `PlayPause`, `Next`…); the most recently active one
still open is followed. Volume buttons change the default output's volume through the
PulseAudio protocol (PipeWire's pulse server), as the official app changes Android's media
volume. Lithidion's own reading.

## 16. Calendar and contacts (LP `calendar/CalendarEvent.kt`, `calendar/PhoneCalendarSyncer.kt`)

**Pins** (BlobDB 1): timeline items (section 12) of type `02`, layout `02` (calendar), parent
the watch's Calendar app (`6c6c6fc2-1912-4d25-8396-3547d1dfac5b`), the start as timestamp, and
the length in minutes. All-day events set flag bit 2, take 0 minutes, and give midnight as if it
were UTC (the watch reads all-day times as local). Attributes: title, location (11), tiny icon
21 (calendar), and headings (25) and paragraphs (26), each a NUL-separated list, for the
description, recurrence and calendar name. Confirmed.

**Reminders** (BlobDB 3): items of type `03`, layout `03`, parent the pin's id, timestamp the
alarm's time; title, location, and tiny icon 3. Lithidion's own reading.

The official app sends two days back and seven ahead. Lithidion keeps a hash of each item sent
per watch, so a resync sends only what changed and deletes what went away.

**On the phone**: Evolution Data Server. Bus names carry an API version
(`org.gnome.evolution.dataserver.Sources5`, `Calendar8`, `AddressBook10`), found from the bus's
names. Sources are an ObjectManager at `/org/gnome/evolution/dataserver/SourceManager`; each
`Source` has `UID` and `Data`, a key file whose `[Calendar]` or `[Address Book]` section gives
its kind and `[Data Source]` its `DisplayName`, `Parent`, and `Enabled`. A factory's
`OpenCalendar`/`OpenAddressBook` (source UID) returns an object path and bus name; `Open` then
`GetObjectList` (an S-expression such as `(occur-in-time-range? (make-time "…Z") (make-time
"…Z"))`) gives iCalendar `VEVENT`s, and `GetContactList` (`(eqphone_national "phone" "…")`)
vCards. Recurrences are expanded on the phone; detached occurrences (`RECURRENCE-ID`) replace
their series' occurrence, and EDS's `/freeassociation.sourceforge.net/` zone prefix is dropped.
Lithidion's own reading, from the first Lithidion's tests on hardware and EDS's documentation.

## 17. Data logging (LP `packets/DataLogging.kt`, `services/DataLoggingService.kt`; PebbleOS `services/data_logging`)

Endpoint 6778, little-endian. The watch opens sessions and sends batches of fixed-size items.
Watch to phone: `01` open (session u8, app UUID, timestamp u32, tag u32, item type u8, item size
u16), `02` data (session u8, items left u32, CRC u32, the items), `03` close (session), `07`
timeout (the watch saw no ACK and keeps the batch), `0A` send-enabled answer. Phone to watch:
`84` report open sessions (a list of session ids; an empty list makes the watch open its
sessions again), `85` ACK and `86` NACK (session), `88` dump all data, `89` get and `8B` set
send-enabled. Confirmed.

Lithidion reports no open sessions at each connection, acknowledges health data once it is
stored (a NACK leaves it on the watch to be sent again), acknowledges and drops other apps' data
(no app here reads it), and refuses everything while the watch runs recovery firmware. The
official app acknowledges before storing.

## 18. Health (LP `health/parsers/HealthDataParser.kt`, `services/HealthService.kt`, `services/HealthStatsSync.kt`, `database/entity/HealthSettings.kt`; PebbleOS `services/activity`, `services/blob_db/health_db.c`, `services/health_sync_endpoint`)

**Logged data** comes from the system (nil UUID) under four tags. Tags 86 (Memfault) and 87
(analytics) are not health data.

- **81, minutes**: each item is a header (version u16, start u32 UTC, local offset in 15-minute
  units u8, sample size u8, sample count u8) and up to 15 one-minute samples: steps u8,
  orientation u8, vector magnitude u16, light u8, flags u8 (bit 0 plugged in, bit 1 active
  minute); from version 6 resting and active calories (1/1000 kcal) and distance (cm), u16 each;
  7 heart rate (bpm, 0 none); 12 heart rate weight u16; 13 heart rate zone u8; 14 SpO2 u8.
  Versions only append; the sample size covers fields a reader does not know. Versions below 5
  lack the flags and are skipped.
- **83, sleep (legacy) and 84, sessions**: version u16 (bit 0 set: fields only ever appended),
  size u16, kind u16 (1 sleep, 2 deep sleep, 3 nap, 4 deep nap, 5 walk, 6 run, 7 workout), UTC to
  local offset i32, start u32, length in seconds u32; from version 3 walks, runs, and workouts add
  steps, active kcal, resting kcal and distance in **metres** (u16 each; LP's parser calls it
  centimetres, PebbleOS's struct says metres). Deep sleep lies within a sleep session.
- **85, heart rate**: a protobuf log of measurements; not kept, as the minutes carry heart rate.

A minute sent again keeps the copy with more steps, and a session the longer copy, as LP does.

**Sync request**, endpoint 911: `01` and seconds since the last sync (u32). The watch flushes all
data logging sessions, then answers `11` with 1 (ACK) or 2 (NACK). Sent at each connection and
whenever the Health tab is opened or refreshed. Confirmed.

**Settings**, BlobDB 7, keys as ASCII without a NUL, sent only to watches advertising
`HealthInsights`:

- `activityPreferences`: height mm u16, weight in 10 g u16, tracking, activity insights, and sleep
  insights (u8 each), age u8, sex u8 (0 female, 1 male, 2 other). The watch's defaults are
  1620 mm, 75.39 kg, age 30, female, everything off.
- `hrmPreferences`: enabled u8; from firmware 4.9.146 the interval u8 (0 ten minutes, 1 thirty,
  2 an hour, 4 five minutes from 4.39); from 4.9.150 measuring during activities u8. The watch
  takes only the size it knows.
- `heartRatePreferences`: resting, elevated, and maximum bpm, then the zone 1 to 3 thresholds (u8
  each). Lithidion sends resting 70, elevated 100, maximum 220 - age, and zones at 50, 70, and 85%
  of the reserve, which give PebbleOS's own defaults at age 30.
- `unitsDistance`: u8, 1 for miles.

The watch has no step goal setting: its Health app compares the day with the typical day below.

**History**, BlobDB 10, keys as ASCII. Values are little-endian u32 words, each starting with
version 1 except the typicals and averages:

- `<weekday>_movementData` (weekday `monday`…`sunday`): local midnight (UTC), steps, active kcal,
  resting kcal, distance in metres (LP sends kilometres), active seconds. The watch ignores days
  older than six days.
- `<weekday>_sleepData`: local midnight, sleep and deep sleep seconds, fell asleep and woke
  (seconds after midnight), then the typical four for that weekday. Midnight 0 updates only the
  typicals, which is how today's goes.
- `<weekday>_heartRateZoneData`: local midnight, a zone count (3), minutes in zones 1 to 3.
- `<weekday>_steps`: 96 u16, the typical steps per quarter hour, `FFFF` unknown.
- `average_dailySteps`, `average_sleepDuration`: u32.

Lithidion sends the six days before today, today's typical sleep, each weekday's typicals (from
the last seven weeks, given at least two days), and the 30-day averages, at the first sync, when
new data arrives (at most hourly) and when the day changes; only what changed goes out. Sleep
for a day is what starts between 6 PM the day before and 2 PM, with gaps under an hour joined,
as LP groups it. Lithidion's own reading.

## 19. App messages and PebbleKit JS (LP `packets/AppMessage.kt`, `connection/endpointmanager/CompanionAppLifecycleManager.kt`, `js/`; mobileapp `pebble/.../ui/WatchappSettingsScreen.kt`)

**App messages**, endpoint 48, little-endian. A push is `01`, a transaction id (u8), the app's
UUID, a tuple count (u8), then the tuples; the receiver answers `FF` (ACK) or `7F` (NACK) with
the same transaction id. A tuple is a u32 key, a type (0 bytes, 1 text with a trailing NUL, 2
unsigned, 3 signed), a u16 length, then the value. Lithidion acknowledges pushes for the running
script and refuses the rest.

**Scripts** are `pebble-js-app.js` in the PBW. As in the official app, one runs at a time: the
script of the app in the watch's foreground, watchfaces included, started and stopped as the
watch's running app changes. Lithidion runs it in a bare JavaScriptCore context (as the iOS app
does) on a thread of its own and supplies `Pebble`, `XMLHttpRequest`, `fetch`, `WebSocket`,
`localStorage`, timers, `navigator.geolocation`, `atob`, `btoa`, and `console`. Message keys are named by
`appKeys` (or `messageKeys`) in `appinfo.json`; scripts may use names or numbers. Booleans go as
signed 0 or 1, numbers as signed unless above `i32::MAX`, arrays as bytes.

- Network requests pass through Lithidion and only while the app's Network Access switch is on
  (off by default); the first refusal in a run asks the user. Only `http` and `https`.
- WebSockets run as tasks in Lithidion under the same switch and close when the script stops.
  `wss` goes anywhere. `ws` goes only to local addresses (loopback, private, link-local, and the
  100.64.0.0/10 shared range Tailscale uses): the name is resolved, other addresses are dropped,
  and the connection is made to an address that was checked. Messages are capped at 8 MiB.
  URLs are parsed as a browser parses them (WHATWG): surrounding spaces are trimmed, the scheme
  may be in any case, and `http`/`https` become `ws`/`wss`.
- Storage is kept per profile and app, and removed with either.
- Location needs `location` in the app's capabilities and goes through the location portal.
- Tokens: MD5 of seed, the app's UUID in upper case, and the official app's salt, in lower-case
  hex. The watch token's seed is the watch's serial; the account token's is a random seed kept
  per profile, as there is no account. (The official app uses the store's developer id in place
  of the UUID when it has one.)
- `showSimpleNotificationOnPebble` goes through Lithidion's notification path under the app's
  name. Timeline tokens, subscriptions, and app glances are unavailable.

**Settings pages**: the script's `showConfiguration` listener calls `Pebble.openURL`; Lithidion
shows the page in WebKitGTK (sandboxed as usual) and treats a navigation to `pebblejs://close`
(`#…`, `/?…`, or `/…`) as the end: the URL-decoded rest goes to the script's `webviewclosed`
listener, or an empty response when the page is closed. Clay pages are `data:` URLs and need
nothing else. Checked with three store apps (Shapes, Hourcast, AWW1): page shown, saved, and the
result sent to the watch.

## 20. Watch settings (LP `database/entity/WatchPrefEntity.kt`, `database/dao/WatchPrefDao.kt`, `connection/endpointmanager/blobdb/BlobDB.kt`; PebbleOS `services/blob_db/endpoint2.c`, `settings_blob_db.c`, `shell/normal/prefs.c`, `prefs_values.h.inc`)

The watch's own settings are records in BlobDB 12, keyed by name. Keys go out with a trailing NUL
and may come back without one; values are little-endian: booleans and enums one byte, numbers u16
or u32, Quiet Time hours four bytes (from hour, minute, to hour, minute), Quick Launch an enabled
byte and the app's UUID (all ones for none). Only keys on PebbleOS's allowlist sync. The watch
stores only settings that were changed; anything it never reports is at its default. Confirmed
from source.

**BlobDB v2** (endpoint 45787, little-endian, responses set bit 7 of the command). Lithidion
sends Version (`0B`, token; answered `8B`, token, status, version) and, for version 1, Dirty All
(`0C`, token, database; only Settings supports it). The watch then sends every stored setting as
Write (`08`) or Writeback (`09`): token, database, last-changed time (u32 seconds), key length
(u8), key, value length (u16), value. Each is answered (`88`/`89`, token, status): Success, or
DataStale when this side changed it later. A sync ends with Sync Done (`0A`, token, database),
answered `8A`. Changes from the phone go as BlobDB v1 Insert With Timestamp (`0D`), which the
watch refuses as DataStale when its own change is newer. Watches without the BlobDB version
capability (section 7) have no settings database.

As the official app does, Lithidion reads first and writes after the watch's Sync Done (or 30 s),
so changes made on the watch while away are kept; it keeps the values per watch, not shared.
Lithidion decides three keys itself: `clock24h` from its System, 12-Hour, or 24-Hour choice
(System follows this device's clock format; a change on the watch fixes the choice),
`unitsWind` (1 km/h, 2 mph) and `unitsTemperature` (1 °C, 2 °F) from its Units.

Model features (PebbleOS board configs): colour backlight on emery; dynamic backlight, backlight on
tap, and Quick View on unsupported faces on emery and gabbro; left-handed on emery, gabbro, and
flint. Text sizes are numbered from 1 on emery and gabbro, 0 elsewhere; "Same as System" for
notifications is 4 on all.

## 21. Dictation (LP `packets/Voice.kt`, `packets/Audio.kt`, `connection/endpointmanager/audio/VoiceSessionManager.kt`; PebbleOS `services/voice/`, `services/voice_endpoint/`, `services/audio_endpoint/`, `include/pbl/services/voice_endpoint_private.h`, `voice/transcription.h`, `applib/voice/dictation_session.c`)

The phone transcribes; the watch only records. An app can start dictation only when the phone
advertises App Dictation (capability bit 7): otherwise `dictation_session_create` returns
nothing and apps report no connection. The official app always advertises it and refuses
sessions with Disabled when speech recognition is off; Lithidion does the same. Confirmed from
source.

**Voice control** (endpoint 11000, little-endian). The watch sends Session Setup (`01`): flags
(u32; bit 0 app-initiated, bit 1 service-retry support), session type (u8: 1 dictation, 2
command, 3 NLP, 4 audio stream for an app's companion), audio session id (u16), and attributes
(count u8, then id u8, length u16, data): 1 Speex info (version, 20 bytes; sample rate u32; bit
rate u16; bitstream version u8; frame size u16), 3 the app's UUID for app sessions. The phone
answers within 8 s with `01`, flags, session type, result (u8: 0 success, 1 service unavailable,
2 timeout, 3 recognizer error, 4 invalid recognizer response, 5 disabled, 6 invalid message);
an audio-stream answer adds the session id. After recording, the phone has 15 s to send
Dictation Result (`02`): flags, session id (u16), result, attributes: 2 the transcription
(type 1, sentence count u8; per sentence a word count u16; per word confidence u8 in percent or
0, length u16, UTF-8 text) and, for an app session, 3 its UUID, without which the watch drops
the result. The watch joins words with spaces (a word starting with backspace, `08`, joins
without one) and rejects words with control characters or no length. A success with no words
counts as an invalid recognizer response ("not recognized").

**Audio** (endpoint 10000). The watch sends Data Transfer (`02`, session id u16, frame count
u8, then each frame as a length byte and the Speex packet) and Stop Transfer (`03`, session id)
when recording ends; the phone may send Stop Transfer to end recording early. PebbleOS
encodes Speex 1.2.1 wideband at 16 kHz, 320-sample frames, complexity 1; it asks for quality 6
and then 9800 bit/s, and the bit rate wins, so the frames are wideband quality 3.

Lithidion accepts dictation sessions only, decodes each frame as it arrives (pure-Rust Speex),
stops recording at 60 s, and transcribes on the device with Parakeet TDT-CTC 110M through
transcribe.cpp (chosen with `tools/voiceprobe`); the model, 135 MB, is downloaded
when dictation is turned on, pinned to a commit and checked against its SHA-256. Command, NLP,
and audio-stream sessions are refused (Disabled; ServiceUnavailable for audio streams).

## 22. Imaging (LP `notification/NotificationImageProvider.kt`; PebbleOS `services/imaging/imaging.c`, `include/pbl/services/imaging_endpoint_types.h`, `services/timeline/notification_layout.c`, `popups/notifications/notification_window.c`, `apps/system/music.c`)

The watch asks the phone for images at the size it will show them, on endpoint 53
(little-endian), when the phone advertises capability bit 17. Only emery and gabbro show them.
Confirmed from source.

**Request** (watch, `01`): token (u8), image type (u8: 0 album art, 1 notification), format
(u8: 0 1-bit, 1 8-bit colour, 2 4-bit palette; the watch asks for 2), width and height (u16),
then for album art the title and artist (pstrings), for a notification the item's UUID (16).

**Response** (phone, `02`): token, flags (u8: 1 first chunk, 2 last, 4 no image, 8 unsupported;
the image type in bits 4–7), offset of the chunk's pixels (u32), and their length (u16), then the
pixels. The first chunk puts an image header before its pixels: width and height (u16), format
(u8), palette count (u8, 1–16 for palette formats), and the palette as GColor8 (two bits each of
alpha, red, green, and blue). Chunks must arrive in order and add up exactly. Rows are padded to
a byte; a 4-bit pixel's left neighbour is in the high nibble. The watch drops images wider or
taller than 300 or larger than 40 kB. Unsupported latches the type off for the connection; no
image resolves the one request.

**Notification pictures.** A notification with attribute 52 (u8, height over width in
sixteenths, clamped to 4–24) gets a band of that shape below its body, and the watch then
requests the picture for the item; without the attribute there is no request. Watches that show
pictures (emery and gabbro) set capability bit 18 in their version response. The attribute must
not go to other watches: firmware that does not know an attribute id can write out of bounds
verifying the item (LP `database/entity/TimelineItem.kt` gates it on bit 18). LP sends the shape
when the notification carries a picture.

Lithidion sends the shape when Pictures in Notifications is on, the watch sets bit 18, and the notification has an `image-data` hint (or its older names); the picture is kept in
memory, scaled to 260 px, for the 32 most recent notifications. It answers with the picture
cropped from the centre to the band's shape, scaled (no more than twice its own size), and
reduced to up to 16 of the watch's 64 colours (each picked for the least remaining error),
with damped Floyd–Steinberg dithering, in chunks of 1000 pixel bytes. Requests for album art
are answered as unsupported.

## 23. Log dump and core dump (LP `packets/LogDump.kt`, `services/LogDumpService.kt`, `packets/GetBytes.kt`, `services/GetBytesService.kt`; PebbleOS `services/log_dump_endpoint/service.c`, `services/get_bytes/get_bytes.c`, `subsys/logging/logging_private.h`)

**Stored log** (endpoint 2002). The phone asks for one generation: `10`, generation (u8),
cookie (u32). The watch copies the cookie back as sent, so its byte order does not matter. Each
line comes as `80`, cookie, then the record: time (u32 BE, UTC seconds), level (u8: 0 always,
1 error, 50 warning, 100 info, 200 debug, 255 verbose), message length (u8), source line (u16
BE), source file (16 bytes, NUL-padded), and the message. `81` + cookie ends the generation.
When the phone advertises capability bit 1, a generation that does not exist is answered `82` +
cookie instead, so the phone can ask until there are none; otherwise it gets `81` and cannot
tell (LP asks for 4). The watch ignores a request while it is sending one. Confirmed from source.

**Core dump** (endpoint 9000, big-endian). `00` + transaction (u8) asks for the latest core
dump; `05` asks only for one not yet read, answering "does not exist" otherwise. The watch
answers `01`, transaction, error (u8: 0 none, 1 malformed, 2 already sending, 3 does not exist,
4 corrupt), size (u32), then `02`, transaction, offset (u32), and data until the size is
reached, in order. A complete read marks the dump read. There is no cancel and no way to learn
the size without the data. The watch reports bit 10 to say it supports `05`. Confirmed from
source.

Lithidion saves either to a file from Advanced: every generation of the log, from 0 up, as
text (`level time file:line> message`, times in the local zone), or the latest core dump
unchanged. It advertises bit 1, waits at most 5 s for each frame, and never fetches a core dump
on its own.
