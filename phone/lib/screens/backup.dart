// The Backup tab: back up now, automatic backup, and how the last one went (the work itself is
// in lib/backup.dart, shared with the background task).

import 'dart:io';

import 'package:flutter/material.dart';
import 'package:intl/intl.dart';
import 'package:photo_manager/photo_manager.dart';

import '../backup.dart';

class BackupPage extends StatefulWidget {
  const BackupPage({super.key, required this.backup, required this.computer});

  /// Null until its settings are loaded.
  final Backup? backup;
  final String computer;

  @override
  State<BackupPage> createState() => _BackupPageState();
}

class _BackupPageState extends State<BackupPage> {
  PermissionState? _permission;
  int _onPhone = 0;

  @override
  void initState() {
    super.initState();
    _load(ask: false);
  }

  Future<void> _load({required bool ask}) async {
    final p = ask ? await PhotoManager.requestPermissionExtend() : await PhotoManager.getPermissionState(requestOption: const PermissionRequestOption());
    var count = 0;
    if (p.hasAccess) {
      final paths = await PhotoManager.getAssetPathList(onlyAll: true, type: RequestType.common);
      if (paths.isNotEmpty) count = await paths.first.assetCountAsync;
    }
    if (mounted) {
      setState(() {
        _permission = p;
        _onPhone = count;
      });
    }
  }

  Future<void> _setAuto(Backup b, bool on) async {
    b.settings.auto = on;
    await b.settings.save();
    await scheduleBackground(on);
    setState(() {});
    if (on && !b.running) b.run(onlyNew: true);
  }

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    final p = _permission;
    final b = widget.backup;
    return ListView(padding: const EdgeInsets.all(20), children: [
      Text('Back up this phone', style: t.titleLarge),
      const SizedBox(height: 8),
      Text(
        "Copies this phone's photos and videos to ${widget.computer}, into Imported, in year and month folders. "
        'Ones already there are skipped, so the next backup only sends new ones.',
        style: t.bodyMedium?.copyWith(color: Colors.white70),
      ),
      const SizedBox(height: 20),
      if (p == null || b == null)
        const Center(child: CircularProgressIndicator())
      else if (!p.hasAccess) ...[
        const Text('Banana Photos needs to see your photos to back them up.'),
        const SizedBox(height: 12),
        FilledButton(onPressed: () => _load(ask: true), child: const Text('Allow access to photos')),
        TextButton(onPressed: PhotoManager.openSetting, child: const Text('Open settings')),
      ] else
        ListenableBuilder(listenable: b, builder: (context, _) => _status(context, b, p)),
    ]);
  }

  Widget _status(BuildContext context, Backup b, PermissionState p) {
    final t = Theme.of(context).textTheme;
    final counts = '${b.checked} of ${b.total} checked · ${b.sent} sent · ${b.skipped} already there${b.failed > 0 ? ' · ${b.failed} failed' : ''}';
    final last = b.settings.lastRun;
    return Column(crossAxisAlignment: CrossAxisAlignment.stretch, children: [
      Text('$_onPhone photos and videos on this phone${p == PermissionState.limited ? ' (only the ones you allowed)' : ''}.', style: t.bodyLarge),
      const SizedBox(height: 8),
      Card(
        child: SwitchListTile(
          title: const Text('Automatic backup'),
          subtitle: Text(Platform.isAndroid
              ? 'New photos go to the computer by themselves: when the app opens, and every so often in the background on Wi-Fi.'
              : 'New photos go to the computer by themselves whenever you open the app.'),
          value: b.settings.auto,
          onChanged: (on) => _setAuto(b, on),
        ),
      ),
      if (last != null && !b.running) ...[
        const SizedBox(height: 4),
        Text(
          'Last backup ${_when(DateTime.fromMillisecondsSinceEpoch(last))}: ${b.settings.lastSent == 0 ? 'nothing new' : '${b.settings.lastSent} sent'}.',
          style: t.bodySmall?.copyWith(color: Colors.white70),
        ),
      ],
      const SizedBox(height: 16),
      if (!b.running)
        FilledButton.icon(onPressed: _onPhone == 0 ? null : () => b.run(), icon: const Icon(Icons.backup), label: const Text('Back up now'))
      else ...[
        LinearProgressIndicator(value: b.total == 0 ? null : b.checked / b.total),
        const SizedBox(height: 8),
        Text(counts),
        if (b.current.isNotEmpty) ...[
          const SizedBox(height: 8),
          Text(b.current, style: t.bodySmall, overflow: TextOverflow.ellipsis),
          LinearProgressIndicator(value: b.fileProgress, minHeight: 2),
        ],
        const SizedBox(height: 8),
        Text('Keep the app open while it runs.', style: t.bodySmall?.copyWith(color: Colors.white70)),
        const SizedBox(height: 8),
        OutlinedButton(onPressed: b.stop, child: const Text('Stop')),
      ],
      if (b.done != null) ...[
        const SizedBox(height: 12),
        Text(b.done!, style: t.bodyLarge),
        Text(counts, style: t.bodySmall),
      ],
      for (final e in b.errors.take(10)) Text(e, style: t.bodySmall?.copyWith(color: Theme.of(context).colorScheme.error)),
    ]);
  }

  String _when(DateTime d) {
    final now = DateTime.now();
    final time = DateFormat.jm().format(d);
    if (d.year == now.year && d.month == now.month && d.day == now.day) return 'today at $time';
    final y = now.subtract(const Duration(days: 1));
    if (d.year == y.year && d.month == y.month && d.day == y.day) return 'yesterday at $time';
    return '${DateFormat.MMMd().format(d)} at $time';
  }
}
