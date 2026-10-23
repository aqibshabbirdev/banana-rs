// The main screen once paired: the computer's photos, search, and backing this phone up.

import 'dart:async';

import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';

import '../api.dart';
import '../backup.dart';
import '../library.dart';
import 'backup.dart';
import 'collections.dart';
import 'photos.dart';
import 'search.dart';

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.pairing, required this.onUnpaired});

  final Pairing pairing;

  /// The computer removed this phone (with why), or the user disconnected (null): back to pairing.
  final Future<void> Function(String? why) onUnpaired;

  @override
  State<HomeScreen> createState() => _HomeScreenState();
}

class _HomeScreenState extends State<HomeScreen> with WidgetsBindingObserver {
  late final Api api = Api(widget.pairing);
  late final Library library = Library(api);

  /// Thumbnails and photos, cached on the phone by id (they never change for an id).
  late final CacheManager cache = CacheManager(Config('banana-${widget.pairing.fingerprint.substring(0, 12)}',
      stalePeriod: const Duration(days: 60), maxNrOfCacheObjects: 40000, fileService: HttpFileService(httpClient: api.client)));
  int _tab = 0;

  /// Not connected yet: "Connecting…", or why the computer can't be reached. Null once connected.
  String? _status = 'Connecting…';
  bool _connecting = false;

  /// While the computer can't be reached, try again now and then, so the phone connects by
  /// itself once Banana Photos is opened there.
  Timer? _retry;

  /// Backing this phone up; null until its settings are read.
  Backup? _backup;
  bool _backupWasRunning = false;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    library.addListener(_onLibrary);
    library.loadSaved();
    loadGridColumns();
    BackupSettings.load(widget.pairing.fingerprint).then((s) {
      if (!mounted) return;
      setState(() => _backup = Backup(api, s)..addListener(_onBackup));
      // Keeps the background task registered (and up to date) while backup is on.
      if (s.auto) scheduleBackground(true);
      if (_status == null) _autoBackup();
    });
    _connect();
  }

  /// With automatic backup on, sends what's new once the computer answers.
  void _autoBackup() {
    final b = _backup;
    if (b != null && b.settings.auto && !b.running && _status == null) b.run(onlyNew: true);
  }

  void _onBackup() {
    final b = _backup!;
    // A backup that sent something just finished: show the new photos.
    if (_backupWasRunning && !b.running && b.sent > 0) library.refresh();
    _backupWasRunning = b.running;
  }

  Future<void> _connect() async {
    if (_connecting) return;
    _connecting = true;
    _retry?.cancel();
    setState(() => _status = 'Connecting to ${widget.pairing.name}…');
    try {
      await api.connect();
      if (!mounted) return;
      setState(() => _status = null);
      await library.refresh(force: true);
      library.startPolling();
      _autoBackup();
    } on ApiException catch (e) {
      if (e.unpaired) return _unpaired();
      _offline(e.message);
    } catch (e) {
      _offline('$e');
    } finally {
      _connecting = false;
    }
  }

  void _offline(String why) {
    if (!mounted) return;
    setState(() => _status = why);
    _retry = Timer(const Duration(seconds: 20), _connect);
  }

  void _onLibrary() {
    final e = library.error;
    if (e is ApiException && e.unpaired) _unpaired();
  }

  Future<void> _unpaired() async {
    if (!mounted) return;
    await _forgetBackup();
    await library.forgetSaved();
    await widget.onUnpaired('${widget.pairing.name} removed this phone. Scan the code on the computer again to reconnect.');
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      if (_status == null) {
        library.refresh();
        _autoBackup();
      } else {
        _connect();
      }
      library.startPolling();
    } else if (state == AppLifecycleState.paused) {
      library.stopPolling();
      _retry?.cancel();
    }
  }

  Future<void> _forgetBackup() async {
    _backup?.stop();
    await scheduleBackground(false);
    await _backup?.settings.forget();
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    _backup?.stop();
    _backup?.dispose();
    _retry?.cancel();
    library.removeListener(_onLibrary);
    library.dispose();
    api.close();
    super.dispose();
  }

  Future<void> _disconnect() async {
    final ok = await showDialog<bool>(
      context: context,
      builder: (c) => AlertDialog(
        title: Text('Disconnect from ${widget.pairing.name}?'),
        content: const Text('This phone forgets the computer. To connect again, scan the code on the computer.'),
        actions: [
          TextButton(onPressed: () => Navigator.pop(c, false), child: const Text('Cancel')),
          TextButton(onPressed: () => Navigator.pop(c, true), child: const Text('Disconnect')),
        ],
      ),
    );
    if (ok == true) {
      await cache.emptyCache();
      await _forgetBackup();
      await library.forgetSaved();
      await widget.onUnpaired(null);
    }
  }

  @override
  Widget build(BuildContext context) {
    final pages = [
      PhotosPage(library: library, api: api, cache: cache),
      SearchPage(library: library, api: api, cache: cache),
      CollectionsPage(library: library, api: api, cache: cache),
      BackupPage(backup: _backup, computer: widget.pairing.name),
    ];
    return Scaffold(
      appBar: AppBar(
        title: Text(widget.pairing.name, overflow: TextOverflow.ellipsis),
        actions: [
          IconButton(tooltip: 'Refresh', onPressed: _status == null ? () => library.refresh(force: true) : _connect, icon: const Icon(Icons.refresh)),
          PopupMenuButton<String>(
            onSelected: (v) => v == 'disconnect' ? _disconnect() : null,
            itemBuilder: (_) => const [PopupMenuItem(value: 'disconnect', child: Text('Disconnect from computer'))],
          ),
        ],
      ),
      body: ListenableBuilder(
        listenable: library,
        builder: (context, _) {
          final status = _status;
          final waiting = status != null && status.endsWith('…');
          // Nothing saved to show yet: the whole screen is about connecting.
          if (status != null && library.items.isEmpty) {
            return Center(
              child: Padding(
                padding: const EdgeInsets.all(32),
                child: Column(mainAxisSize: MainAxisSize.min, children: [
                  if (waiting) const CircularProgressIndicator() else const Icon(Icons.wifi_off, size: 48),
                  const SizedBox(height: 16),
                  Text(status, textAlign: TextAlign.center),
                  if (!waiting) ...[
                    const SizedBox(height: 16),
                    FilledButton(onPressed: _connect, child: const Text('Try again')),
                  ],
                ]),
              ),
            );
          }
          final tabs = IndexedStack(index: _tab, children: pages);
          if (status == null) return tabs;
          // The saved timeline, with a strip saying it isn't connected (yet).
          return Column(children: [
            Material(
              color: waiting ? Theme.of(context).colorScheme.surfaceContainerHigh : Theme.of(context).colorScheme.errorContainer,
              child: Padding(
                padding: const EdgeInsets.fromLTRB(16, 6, 8, 6),
                child: Row(children: [
                  if (waiting)
                    const SizedBox(width: 16, height: 16, child: CircularProgressIndicator(strokeWidth: 2))
                  else
                    const Icon(Icons.wifi_off, size: 18),
                  const SizedBox(width: 10),
                  Expanded(
                    child: Text(
                      waiting ? 'Connecting to ${widget.pairing.name}… · showing saved photos' : "Can't reach ${widget.pairing.name} · showing saved photos. Is Banana Photos open there?",
                      style: Theme.of(context).textTheme.bodySmall,
                    ),
                  ),
                  if (!waiting) TextButton(onPressed: _connect, child: const Text('Retry')),
                ]),
              ),
            ),
            Expanded(child: tabs),
          ]);
        },
      ),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.photo_outlined), selectedIcon: Icon(Icons.photo), label: 'Photos'),
          NavigationDestination(icon: Icon(Icons.search), label: 'Search'),
          NavigationDestination(icon: Icon(Icons.photo_album_outlined), selectedIcon: Icon(Icons.photo_album), label: 'Albums'),
          NavigationDestination(icon: Icon(Icons.backup_outlined), selectedIcon: Icon(Icons.backup), label: 'Backup'),
        ],
      ),
    );
  }
}
