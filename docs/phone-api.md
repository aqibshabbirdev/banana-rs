# Phone app API

The phone app (Flutter, Android and iOS) shows a computer's Banana Photos library and backs the phone's photos up to it, over the local network only. The computer side is `src/remote/`. Everything here is tested end to end in `remote::tests::phone_api_end_to_end`.

## 1. Pairing

On the computer, **Connect phone** → *Let phones on this network connect* shows a QR code. It holds a link:

```
bananaphotos://pair?v=1&n=Aqib%27s%20PC&h=192.168.1.5%2C10.0.0.2&p=47820&m=47821&c=3f9a0c1d2e4b5a67&f=<64 hex>
```

| Field | Meaning |
|---|---|
| `v` | Pairing format version (1) |
| `n` | Computer name, percent-encoded |
| `h` | The computer's private IPv4 addresses, comma-separated, home Wi-Fi first. Try them in order. |
| `p` | HTTPS port (47820) |
| `m` | Media port, plain HTTP, for video players (47821) |
| `c` | One-time pairing code. Works once, for 10 minutes; 5 wrong tries end it. |
| `f` | SHA-256 of the computer's TLS certificate (DER), lowercase hex. **The only certificate the app may trust.** |

Then:

```
POST https://<h>:<p>/api/pair
Content-Type: application/json

{"code": "<c>", "name": "Aqib's Pixel 7"}
```

→ `200 {"token": "<64 hex>", "name": "Aqib's PC", "fingerprint": "<f>"}`, or `403` for a wrong or used code.

Keep `token`, `fingerprint`, the name and the last address that worked (Keychain on iOS, EncryptedSharedPreferences / Keystore on Android). Send the token on every other request:

```
Authorization: Bearer <token>
```

`401` means the phone was removed on the computer (or never paired): show "Scan the QR code again".

### Trusting the computer (certificate pinning)

The certificate is self-signed, so normal TLS validation fails. Accept it **only** when its SHA-256 equals the paired fingerprint:

```dart
import 'dart:io';
import 'package:crypto/crypto.dart';

HttpClient pinnedClient(String fingerprint) {
  final client = HttpClient(context: SecurityContext(withTrustedRoots: false));
  client.badCertificateCallback = (X509Certificate cert, String host, int port) =>
      sha256.convert(cert.der).toString() == fingerprint;
  return client;
}
```

Use this client (e.g. with `IOClient` from `package:http`) for everything on port `p`, images included (`Image` from bytes, or a cache manager built on this client). Never set `badCertificateCallback` to accept everything.

### Finding the computer again

The computer announces itself over mDNS (Bonjour) as `_bananaphotos._tcp`, instance name = computer name, with TXT records `v=1`, `m=<media port>`, `f=<fingerprint>`. When the saved address stops answering, browse for that service and use the entry whose `f` matches the saved fingerprint (packages: `nsd` or `bonsoir`).

### Platform settings

- **iOS** `Info.plist`: `NSLocalNetworkUsageDescription` (why: "to find and connect to Banana Photos on your computer"), `NSBonjourServices` = `["_bananaphotos._tcp"]`, and `NSAppTransportSecurity` → `NSAllowsLocalNetworking = true` (for the plain-HTTP media port).
- **Android**: the media port is plain HTTP, which Android blocks by default. Allow cleartext with a `network_security_config` (cleartext permitted; it only ever carries signed, short-lived links on the LAN). Permissions: `INTERNET`, `ACCESS_WIFI_STATE`, `CHANGE_WIFI_MULTICAST_STATE` (mDNS), and for backup `READ_MEDIA_IMAGES` / `READ_MEDIA_VIDEO` (Android 13+) or `READ_EXTERNAL_STORAGE`.

## 2. Browsing

All JSON. Times (`taken`, `expires`) are **seconds since 1970 of the local wall-clock time, written as if it were UTC**: show them with a UTC formatter and the date matches what the camera wrote. Ids are 16 hex characters and stay the same for a file across restarts.

### `GET /api/info`

```json
{"name": "Aqib's PC", "version": "0.1.3", "api": 1, "library_version": 12, "count": 21034, "ready": true}
```

`ready` is false while no library folder is open. `library_version` goes up whenever the library changes (scan, upload, favourite); refetch the list when it differs from yours.

### `GET /api/photos?offset=0&limit=1000`

Newest first, at most 5000 per page. Archived photos are left out; Locked Folder is never shared.

```json
{
  "library_version": 12, "total": 21034, "offset": 0,
  "items": [
    {"id": "9f3b2c1d00aa7e41", "name": "IMG_2104.HEIC", "taken": 1729420215, "aspect": 0.75,
     "size": 2381904, "video": false, "favorite": true},
    {"id": "0c4d8e2f11b3a9d0", "name": "VID_0042.mp4", "taken": 1729401002, "aspect": 0.563,
     "size": 48213377, "video": true, "duration": 31.5, "favorite": false}
  ]
}
```

`aspect` is width ÷ height as displayed (rotation applied): use it to lay out the grid before thumbnails arrive.

### `GET /api/thumb/<id>`

The computer's 320-pixel JPEG thumbnail (photos and video frames). Sent with `Cache-Control: immutable` and an `ETag`: cache by id.

### `GET /api/photo/<id>?max=2048`

The photo upright as JPEG, at most `max` pixels on the long side (64–8192, default 2048). HEIC, RAW and the rest all come out as JPEG. `?original=1` sends the file exactly as stored (supports `Range`).

### `GET /api/search?q=<text>`

```json
{"kind": "content", "ids": ["9f3b2c1d00aa7e41", "..."]}
```

`kind` is `name` (file name or date words such as `2024`, `october`, `sunday`, `2024-10-06`), `content` (by what's in the photos, best first; uses the computer's CLIP model) or `none`. Content search only covers photos the computer has analysed.

### `POST /api/favorite/<id>?on=1`

`on=0` removes the favourite. → `{"favorite": true}`.

### `POST /api/favorites` and `POST /api/trash`

Several at once, with a JSON body `{"ids": ["...", "..."]}` (favorites also takes `"on": true|false`). Unknown ids are ignored.

- favorites → `{"changed": 2}`
- trash moves the files to the computer's Trash (Finder's on macOS, the freedesktop one on Linux, where they can be put back), leaving ones inside an Apple Photos library alone → `{"trashed": 1, "skipped": 0, "errors": []}`. The computer remembers what was deleted, so `/api/has` says `true` for it and a phone's backup doesn't send it back.

### Albums and people

`GET /api/albums` → `{"albums": [{"id": "0", "name": "Trip", "count": 42, "cover": "<photo id>"}]}`: the albums made on the computer.

`GET /api/people` → `{"people": [{"id": "9f3b2c1d00aa7e41", "name": "Sara", "count": 120, "cover": "<photo id>"}]}`: the people the computer recognised (named first, then by how many photos; hidden ones left out). `name` is `null` for someone not named yet.

`GET /api/album/<id>` and `GET /api/person/<id>` → `{"name": "...", "ids": ["...", "..."]}`: their photos, newest first.

`GET /api/face/<person id>`: the person's round-avatar picture (a small square JPEG), `404` when there isn't one yet (show `cover` instead).

Favourites need no call: they're the items with `"favorite": true`.

## 3. Video

`GET /api/video/<id>` streams the file with `Range` support, over the pinned HTTPS client.

Native players (ExoPlayer, AVPlayer, which `video_player` uses) can't be given the pin or the token. For them:

```
GET /api/media-url/<id>  →  {"url": "http://192.168.1.5:47821/m/0c4d8e2f11b3a9d0?e=1729450000&s=<hmac>", "expires": 1729450000}
```

Open `url` in the player as is. It's plain HTTP on the media port, needs no header, works for that one file only, and expires after 6 hours (ask again then). Links stop working when the computer restarts the server.

## 4. Backup (phone → computer)

For each photo or video in the phone's gallery (`photo_manager`):

1. `GET /api/has?name=<file name>&size=<bytes>&taken=<seconds>` → `{"have": true|false}`. Skip it when `true`. It's also `true` for a file of that name and size anywhere in the library (one the phone saved from the computer), and such an upload is skipped.
2. Otherwise `POST /api/upload?name=<file name>&taken=<seconds>` with the raw file as the body and `Content-Length` set (no chunked uploads):

   → `{"status": "imported", "name": "IMG_0001.jpg"}` or `{"status": "skipped"}` (already there).

Files land in the computer's library under `Imported/<phone name>/<year>/<month>/`, dated by their EXIF date when they have one, else by `taken`. Errors: `411` no length, `413` over 16 GB, `415` not a photo or video, `503` no library folder open. Send files one at a time; the computer re-scans a few seconds after the last one.

On iOS, background time is short: back up while the app is open, and continue in a `BGProcessingTask` when iOS allows. On Android, a `WorkManager` job on unmetered Wi-Fi.

## Errors

Every error is `{"error": "<message for people>"}` with the status: `400` bad request, `401` not paired, `403` wrong code or bad link, `404` unknown id or route, `411`, `413`, `415`, `416` bad range, `503` no library.
