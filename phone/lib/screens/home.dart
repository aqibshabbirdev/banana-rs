// The main screen once paired: the computer's photos, search, and backing this phone up.

import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';

import '../api.dart';
import '../library.dart';
import 'backup.dart';
import 'photos.dart';
import 'search.dart';

class HomeScreen extends StatefulWidget {
  const HomeScreen({super.key, required this.pairing, required this.onUnpaired});

  final Pairing pairing;

  /// The computer removed this phone, or the user disconnected: back to pairing.
  final Future<void> Function() onUnpaired;

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
  String? _status = 'Connecting…';

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    library.addListener(_onLibrary);
    _connect();
  }

  Future<void> _connect() async {
    setState(() => _status = 'Connecting to ${widget.pairing.name}…');
    try {
      await api.connect();
      setState(() => _status = null);
      await library.refresh(force: true);
      library.startPolling();
    } on ApiException catch (e) {
      if (e.unpaired) return _unpaired();
      setState(() => _status = e.message);
    } catch (e) {
      setState(() => _status = '$e');
    }
  }

  void _onLibrary() {
    final e = library.error;
    if (e is ApiException && e.unpaired) _unpaired();
  }

  Future<void> _unpaired() async {
    if (!mounted) return;
    ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('${widget.pairing.name} removed this phone. Scan the code again to reconnect.')));
    await widget.onUnpaired();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) {
      _status == null ? library.refresh() : _connect();
      library.startPolling();
    } else if (state == AppLifecycleState.paused) {
      library.stopPolling();
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
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
      await widget.onUnpaired();
    }
  }

  @override
  Widget build(BuildContext context) {
    final pages = [
      PhotosPage(library: library, api: api, cache: cache),
      SearchPage(library: library, api: api, cache: cache),
      BackupPage(api: api, library: library),
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
      body: _status != null
          ? Center(
              child: Padding(
                padding: const EdgeInsets.all(32),
                child: Column(mainAxisSize: MainAxisSize.min, children: [
                  if (_status!.endsWith('…')) const CircularProgressIndicator() else const Icon(Icons.wifi_off, size: 48),
                  const SizedBox(height: 16),
                  Text(_status!, textAlign: TextAlign.center),
                  if (!_status!.endsWith('…')) ...[
                    const SizedBox(height: 16),
                    FilledButton(onPressed: _connect, child: const Text('Try again')),
                  ],
                ]),
              ),
            )
          : IndexedStack(index: _tab, children: pages),
      bottomNavigationBar: NavigationBar(
        selectedIndex: _tab,
        onDestinationSelected: (i) => setState(() => _tab = i),
        destinations: const [
          NavigationDestination(icon: Icon(Icons.photo_outlined), selectedIcon: Icon(Icons.photo), label: 'Photos'),
          NavigationDestination(icon: Icon(Icons.search), label: 'Search'),
          NavigationDestination(icon: Icon(Icons.backup_outlined), selectedIcon: Icon(Icons.backup), label: 'Backup'),
        ],
      ),
    );
  }
}
