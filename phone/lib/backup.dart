// Backing this phone's photos and videos up to the computer, newest first. Each one is checked
// with /api/has first, so only what the computer doesn't have is sent. With automatic backup on,
// it runs when the app opens and, on Android, now and then in the background (WorkManager, on
// Wi-Fi); those runs only look at what was added or changed since the last finished pass.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter/foundation.dart';
import 'package:path_provider/path_provider.dart';
import 'package:photo_manager/photo_manager.dart';
import 'package:workmanager/workmanager.dart';

import 'api.dart';

/// Backup settings and where the last pass got to, kept per computer.
class BackupSettings {
  BackupSettings._(this._file, Map<String, dynamic> j)
      : auto = j['auto'] as bool? ?? false,
        doneUntil = j['done_until'] as int?,
        lastRun = j['last_run'] as int?,
        lastSent = j['last_sent'] as int? ?? 0;

  final File _file;
  bool auto;

  /// When the last pass that went through everything (with nothing failing) started, in
  /// milliseconds: later passes only need to look at what was added or changed after it.
  int? doneUntil;

  /// When a backup last finished, and how many it sent then.
  int? lastRun;
  int lastSent;

  static Future<BackupSettings> load(String fingerprint) async {
    final f = File('${(await getApplicationSupportDirectory()).path}/backup-${fingerprint.substring(0, 12)}.json');
    Map<String, dynamic> j = {};
    try {
      j = jsonDecode(await f.readAsString()) as Map<String, dynamic>;
    } catch (_) {}
    return BackupSettings._(f, j);
  }

  Future<void> save() async {
    await _file.parent.create(recursive: true);
    await _file.writeAsString(jsonEncode({'auto': auto, 'done_until': doneUntil, 'last_run': lastRun, 'last_sent': lastSent}));
  }

  Future<void> forget() async {
    try {
      await _file.delete();
    } catch (_) {}
  }
}

/// One backup at a time across the app and the background task: whoever runs keeps this file's
/// time fresh.
class _Lock {
  _Lock(this._file);
  final File _file;
  Timer? _beat;

  static Future<_Lock> open() async => _Lock(File('${(await getApplicationSupportDirectory()).path}/backup.lock'));

  Future<bool> take() async {
    try {
      final t = int.parse(await _file.readAsString());
      if (DateTime.now().millisecondsSinceEpoch - t < 60000) return false;
    } catch (_) {}
    await _touch();
    _beat = Timer.periodic(const Duration(seconds: 15), (_) => _touch());
    return true;
  }

  Future<void> _touch() async {
    try {
      await _file.writeAsString('${DateTime.now().millisecondsSinceEpoch}');
    } catch (_) {}
  }

  Future<void> release() async {
    _beat?.cancel();
    try {
      await _file.delete();
    } catch (_) {}
  }
}

class Backup extends ChangeNotifier {
  Backup(this.api, this.settings);

  final Api api;
  final BackupSettings settings;

  bool running = false;
  bool _stop = false;

  /// This pass: how many it looks at, and what happened to them.
  int total = 0, checked = 0, sent = 0, skipped = 0, failed = 0;
  String current = '';
  double fileProgress = 0;
  final List<String> errors = [];

  /// How the last pass ended ("Done. …"), until the next one starts.
  String? done;

  /// Runs a pass. [onlyNew] looks only at what changed since the last finished pass (all of
  /// them the first time); [limit] stops it after that long (background tasks get ~10 minutes).
  Future<void> run({bool onlyNew = false, Duration? limit}) async {
    if (running) return;
    final lock = await _Lock.open();
    if (!await lock.take()) return; // The other one (app or background) is at it.
    final started = DateTime.now();
    running = true;
    _stop = false;
    total = checked = sent = skipped = failed = 0;
    errors.clear();
    done = null;
    notifyListeners();
    try {
      final since = onlyNew ? settings.doneUntil : null;
      // What arrived since then (with a day of slack for clocks): on Android the create time is
      // when the file was added to the phone; on iOS adding one also sets its modification date.
      final recent = since == null
          ? null
          : DateTimeCond(min: DateTime.fromMillisecondsSinceEpoch(since).subtract(const Duration(days: 1)), max: DateTime.now().add(const Duration(days: 1)));
      final filter = FilterOptionGroup(orders: [const OrderOption(type: OrderOptionType.createDate, asc: false)]);
      if (recent != null && Platform.isAndroid) filter.createTimeCond = recent;
      if (recent != null && !Platform.isAndroid) {
        filter.createTimeCond = DateTimeCond.def().copyWith(ignore: true);
        filter.updateTimeCond = recent;
      }
      final paths = await PhotoManager.getAssetPathList(onlyAll: true, type: RequestType.common, filterOption: filter);
      if (paths.isNotEmpty) {
        final all = paths.first;
        total = await all.assetCountAsync;
        notifyListeners();
        const pageSize = 50;
        for (var page = 0; page * pageSize < total && !_stop; page++) {
          final assets = await all.getAssetListPaged(page: page, size: pageSize);
          for (final a in assets) {
            if (limit != null && DateTime.now().difference(started) > limit) _stop = true;
            if (_stop) break;
            await _one(a);
          }
        }
      }
      final finished = !_stop;
      if (finished && failed == 0) settings.doneUntil = started.millisecondsSinceEpoch;
      settings.lastRun = DateTime.now().millisecondsSinceEpoch;
      settings.lastSent = sent;
      await settings.save();
      done = finished ? 'Done. ${sent == 0 ? 'Everything was already on the computer.' : '$sent sent to the computer.'}' : 'Stopped.';
    } catch (e) {
      errors.add('$e');
      done = 'Stopped.';
    } finally {
      running = false;
      current = '';
      await lock.release();
      notifyListeners();
    }
  }

  void stop() {
    _stop = true;
    notifyListeners();
  }

  bool _disposed = false;

  // A pass still finishing after the screen went away (stopped) mustn't notify.
  @override
  void notifyListeners() {
    if (!_disposed) super.notifyListeners();
  }

  @override
  void dispose() {
    _disposed = true;
    super.dispose();
  }

  Future<void> _one(AssetEntity a) async {
    try {
      final name = await a.titleAsync;
      final taken = wallClockSeconds(a.createDateTime);
      current = name;
      fileProgress = 0;
      notifyListeners();
      // Ask the computer first: no need to send a file it has.
      final file = await a.originFile;
      if (file == null) throw Exception('not available on this phone');
      final size = await file.length();
      if (await api.has(name, size, taken)) {
        skipped++;
      } else {
        final status = await api.upload(file, name, taken, progress: (s, t) {
          fileProgress = t == 0 ? 0 : s / t;
          notifyListeners();
        });
        status == 'imported' ? sent++ : skipped++;
      }
    } on ApiException catch (e) {
      failed++;
      errors.add(e.message);
      if (e.unpaired || e.status == 503) _stop = true;
    } catch (e) {
      failed++;
      errors.add('$e');
    } finally {
      checked++;
      notifyListeners();
    }
  }
}

// --- Android: in the background -------------------------------------------------------------

const _task = 'banana-backup';

/// Turns the background backup on or off (Android; iOS only backs up while the app is open).
Future<void> scheduleBackground(bool on) async {
  if (!Platform.isAndroid) return;
  try {
    await Workmanager().initialize(backgroundMain);
    if (on) {
      await Workmanager().registerPeriodicTask(
        _task,
        _task,
        frequency: const Duration(minutes: 30),
        constraints: Constraints(networkType: NetworkType.unmetered, requiresBatteryNotLow: true),
        existingWorkPolicy: ExistingPeriodicWorkPolicy.update,
      );
      // Testing (flutter build --dart-define=BANANA_TEST_BACKGROUND=true): the same task once,
      // a minute from now, since a periodic one can't be run early.
      if (const bool.fromEnvironment('BANANA_TEST_BACKGROUND')) {
        await Workmanager().registerOneOffTask('$_task-test', _task, initialDelay: const Duration(seconds: 60));
      }
    } else {
      await Workmanager().cancelByUniqueName(_task);
    }
  } catch (e) {
    debugPrint('background backup: $e');
  }
}

/// Where WorkManager starts the app's code, without the UI.
@pragma('vm:entry-point')
void backgroundMain() {
  Workmanager().executeTask((task, input) async {
    final p = await Pairing.load();
    if (p == null || p.token == null) return true;
    final settings = await BackupSettings.load(p.fingerprint);
    if (!settings.auto) return true;
    // Access was granted in the app; there is no screen here to ask again.
    PhotoManager.setIgnorePermissionCheck(true);
    final api = Api(p);
    try {
      await api.connect();
      await Backup(api, settings).run(onlyNew: true, limit: const Duration(minutes: 8));
    } catch (_) {
      // Not on the computer's network, or Banana Photos isn't open there: next time.
    } finally {
      api.close();
    }
    return true;
  });
}
