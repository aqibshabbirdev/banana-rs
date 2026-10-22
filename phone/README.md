# Banana Photos for phones

The phone side of **Connect phone**: see the library on your computer, and back this phone up to it, over your own Wi-Fi. Android and iOS, Flutter.

- `lib/api.dart`: pairing link, certificate pinning, finding the computer again over mDNS, and the API ([../docs/phone-api.md](../docs/phone-api.md))
- `lib/library.dart`: the timeline, grouped by day
- `lib/screens/`: pairing (`connect.dart`), the tabs (`home.dart`), timeline (`photos.dart`), viewer and video (`viewer.dart`), search, backup

Build and test: see "Phone app" in the [main README](../README.md#phone-app).
