// Backing this phone's photos and videos up to the computer, newest first. Each one is checked
// with /api/has first, so a backup after the first only sends what's new. Runs while the app
// is open (iOS gives apps very little time in the background).

import 'package:flutter/material.dart';
import 'package:photo_manager/photo_manager.dart';

import '../api.dart';
import '../library.dart';

class BackupPage extends StatefulWidget {
  const BackupPage({super.key, required this.api, required this.library});

  final Api api;
  final Library library;

  @override
  State<BackupPage> createState() => _BackupPageState();
}

class _BackupPageState extends State<BackupPage> {
  PermissionState? _permission;
  AssetPathEntity? _all;
  int _total = 0;

  bool _running = false;
  bool _stop = false;
  int _checked = 0, _sent = 0, _skipped = 0, _failed = 0;
  String _current = '';
  double _fileProgress = 0;
  final List<String> _errors = [];
  String? _done;

  @override
  void initState() {
    super.initState();
    _load(ask: false);
  }

  Future<void> _load({required bool ask}) async {
    final p = ask ? await PhotoManager.requestPermissionExtend() : await PhotoManager.getPermissionState(requestOption: const PermissionRequestOption());
    AssetPathEntity? all;
    var total = 0;
    if (p.hasAccess) {
      final paths = await PhotoManager.getAssetPathList(
        onlyAll: true,
        type: RequestType.common,
        filterOption: FilterOptionGroup(orders: [const OrderOption(type: OrderOptionType.createDate, asc: false)]),
      );
      if (paths.isNotEmpty) {
        all = paths.first;
        total = await all.assetCountAsync;
      }
    }
    if (mounted) {
      setState(() {
        _permission = p;
        _all = all;
        _total = total;
      });
    }
  }

  Future<void> _run() async {
    final all = _all;
    if (all == null) return;
    setState(() {
      _running = true;
      _stop = false;
      _checked = _sent = _skipped = _failed = 0;
      _errors.clear();
      _done = null;
    });
    const pageSize = 50;
    for (var page = 0; page * pageSize < _total && !_stop; page++) {
      final assets = await all.getAssetListPaged(page: page, size: pageSize);
      for (final a in assets) {
        if (_stop) break;
        await _one(a);
      }
    }
    if (mounted) {
      setState(() {
        _running = false;
        _current = '';
        _done = _stop ? 'Stopped.' : 'Done. ${_sent == 0 ? 'Everything was already on the computer.' : '$_sent sent to the computer.'}';
      });
    }
    if (_sent > 0) {
      // The computer re-scans a few seconds after the last upload.
      Future.delayed(const Duration(seconds: 6), () => widget.library.refresh());
    }
  }

  Future<void> _one(AssetEntity a) async {
    try {
      final name = await a.titleAsync;
      final taken = wallClockSeconds(a.createDateTime);
      setState(() {
        _current = name;
        _fileProgress = 0;
      });
      // Ask the computer first: no need to fetch an iCloud original or send a file it has.
      final file = await a.originFile;
      if (file == null) throw Exception('not available on this phone');
      final size = await file.length();
      if (await widget.api.has(name, size, taken)) {
        _skipped++;
      } else {
        final status = await widget.api.upload(file, name, taken, progress: (s, t) {
          if (mounted) setState(() => _fileProgress = t == 0 ? 0 : s / t);
        });
        status == 'imported' ? _sent++ : _skipped++;
      }
    } on ApiException catch (e) {
      _failed++;
      _errors.add(e.message);
      if (e.unpaired || e.status == 503) _stop = true;
    } catch (e) {
      _failed++;
      _errors.add('$e');
    } finally {
      _checked++;
      if (mounted) setState(() {});
    }
  }

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    final p = _permission;
    return ListView(padding: const EdgeInsets.all(20), children: [
      Text('Back up this phone', style: t.titleLarge),
      const SizedBox(height: 8),
      Text(
        "Copies this phone's photos and videos to ${widget.api.pairing.name}, into Imported, in year and month folders. "
        'Ones already there are skipped, so the next backup only sends new ones. Keep the app open while it runs.',
        style: t.bodyMedium?.copyWith(color: Colors.white70),
      ),
      const SizedBox(height: 20),
      if (p == null)
        const Center(child: CircularProgressIndicator())
      else if (!p.hasAccess) ...[
        const Text('Banana Photos needs to see your photos to back them up.'),
        const SizedBox(height: 12),
        FilledButton(onPressed: () => _load(ask: true), child: const Text('Allow access to photos')),
        TextButton(onPressed: PhotoManager.openSetting, child: const Text('Open settings')),
      ] else ...[
        Text('$_total photos and videos on this phone${p == PermissionState.limited ? ' (only the ones you allowed)' : ''}.', style: t.bodyLarge),
        const SizedBox(height: 16),
        if (!_running)
          FilledButton.icon(onPressed: _total == 0 ? null : _run, icon: const Icon(Icons.backup), label: const Text('Back up now'))
        else ...[
          LinearProgressIndicator(value: _total == 0 ? null : _checked / _total),
          const SizedBox(height: 8),
          Text('$_checked of $_total checked · $_sent sent · $_skipped already there${_failed > 0 ? ' · $_failed failed' : ''}'),
          if (_current.isNotEmpty) ...[
            const SizedBox(height: 8),
            Text(_current, style: t.bodySmall, overflow: TextOverflow.ellipsis),
            LinearProgressIndicator(value: _fileProgress, minHeight: 2),
          ],
          const SizedBox(height: 8),
          OutlinedButton(onPressed: () => setState(() => _stop = true), child: const Text('Stop')),
        ],
        if (_done != null) ...[
          const SizedBox(height: 12),
          Text(_done!, style: t.bodyLarge),
          Text('$_checked checked · $_sent sent · $_skipped already there${_failed > 0 ? ' · $_failed failed' : ''}', style: t.bodySmall),
        ],
        for (final e in _errors.take(10)) Text(e, style: t.bodySmall?.copyWith(color: Theme.of(context).colorScheme.error)),
      ],
    ]);
  }
}
