// The computer's library as the phone holds it: the timeline (newest first), grouped by day,
// refreshed when the computer says it changed. It's also saved on the phone, so the app opens
// on the last timeline it saw even when the computer is off (thumbnails come from the cache).

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';

import 'api.dart';

class Item {
  Item.fromJson(Map<String, dynamic> j)
      : id = j['id'] as String,
        name = j['name'] as String,
        taken = fromWallClockSeconds(j['taken'] as int),
        aspect = (j['aspect'] as num).toDouble(),
        size = j['size'] as int,
        video = j['video'] as bool,
        duration = (j['duration'] as num?)?.toDouble(),
        favorite = j['favorite'] as bool;

  final String id;
  final String name;

  /// Wall-clock time the photo was taken (fields are local time; see `fromWallClockSeconds`).
  final DateTime taken;
  final double aspect;
  final int size;
  final bool video;
  final double? duration;
  bool favorite;

  DateTime get day => DateTime.utc(taken.year, taken.month, taken.day);
}

/// One row of the timeline list: a day heading, or up to `columns` photos of one day.
sealed class TimelineRow {}

class DayRow extends TimelineRow {
  DayRow(this.day, this.count);
  final DateTime day;
  final int count;
}

class PhotosRow extends TimelineRow {
  PhotosRow(this.start, this.end);

  /// Range in `Library.items`.
  final int start, end;
}

class Library extends ChangeNotifier {
  Library(this.api);

  final Api api;
  List<Item> items = [];
  Map<String, Item> byId = {};
  int version = -1;
  bool loading = false;
  Object? error;
  Timer? _poll;

  /// Fetches the timeline if the computer's library changed since the last time.
  Future<void> refresh({bool force = false}) async {
    if (loading) return;
    loading = true;
    error = null;
    notifyListeners();
    try {
      final info = await api.info();
      if (force || info['library_version'] != version) {
        final got = await api.photos();
        _set(got.items, got.version);
        unawaited(_save(got.items, got.version));
      }
    } catch (e) {
      error = e;
    } finally {
      loading = false;
      notifyListeners();
    }
  }

  void _set(List<Map<String, dynamic>> raw, int v) {
    items = raw.map(Item.fromJson).toList();
    byId = {for (final i in items) i.id: i};
    version = v;
  }

  Future<File> _file() async =>
      File('${(await getApplicationSupportDirectory()).path}/library-${api.pairing.fingerprint.substring(0, 12)}.json');

  /// The timeline saved the last time the computer was reached, if any.
  Future<void> loadSaved() async {
    try {
      final j = jsonDecode(await (await _file()).readAsString()) as Map<String, dynamic>;
      if (items.isEmpty) {
        _set((j['items'] as List).cast<Map<String, dynamic>>(), j['version'] as int);
        notifyListeners();
      }
    } catch (_) {
      // Nothing saved yet, or from an older app: wait for the computer.
    }
  }

  Future<void> _save(List<Map<String, dynamic>> raw, int v) async {
    try {
      final f = await _file();
      final tmp = File('${f.path}.tmp');
      await tmp.writeAsString(jsonEncode({'version': v, 'items': raw}));
      await tmp.rename(f.path);
    } catch (_) {}
  }

  /// Removes the saved timeline (when the phone is unpaired).
  Future<void> forgetSaved() async {
    try {
      await (await _file()).delete();
    } catch (_) {}
  }

  /// Checks for changes every 30 seconds while the app is open.
  void startPolling() {
    _poll?.cancel();
    _poll = Timer.periodic(const Duration(seconds: 30), (_) => refresh());
  }

  void stopPolling() => _poll?.cancel();

  Future<void> setFavorite(Item item, bool on) async {
    item.favorite = on;
    notifyListeners();
    try {
      await api.favorite(item.id, on);
    } catch (e) {
      item.favorite = !on;
      notifyListeners();
      rethrow;
    }
  }

  @override
  void dispose() {
    _poll?.cancel();
    super.dispose();
  }
}

/// The timeline as list rows: each day's heading, then its photos `columns` to a row.
List<TimelineRow> layoutRows(List<Item> items, int columns) {
  final rows = <TimelineRow>[];
  var i = 0;
  while (i < items.length) {
    final day = items[i].day;
    var end = i;
    while (end < items.length && items[end].day == day) {
      end++;
    }
    rows.add(DayRow(day, end - i));
    for (var s = i; s < end; s += columns) {
      rows.add(PhotosRow(s, s + columns < end ? s + columns : end));
    }
    i = end;
  }
  return rows;
}
