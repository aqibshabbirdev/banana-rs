// The Albums tab: Favourites, the people the computer recognised, the places photos were taken
// in, and the albums made there.

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';

import '../api.dart';
import '../library.dart';
import 'photos.dart';

class CollectionsPage extends StatefulWidget {
  const CollectionsPage({super.key, required this.library, required this.api, required this.cache});

  final Library library;
  final Api api;
  final CacheManager cache;

  @override
  State<CollectionsPage> createState() => _CollectionsPageState();
}

class _CollectionsPageState extends State<CollectionsPage> {
  List<Map<String, dynamic>>? _albums, _people, _places;
  Object? _error;

  /// The library version they were fetched for: fetched again when the computer's changes.
  int _for = -2;
  bool _loading = false;

  @override
  void initState() {
    super.initState();
    widget.library.addListener(_changed);
    _changed();
  }

  @override
  void dispose() {
    widget.library.removeListener(_changed);
    super.dispose();
  }

  void _changed() {
    if (widget.library.version != _for && widget.library.error == null) _load();
  }

  Future<void> _load() async {
    if (_loading) return;
    _loading = true;
    final version = widget.library.version;
    try {
      final got = await Future.wait([widget.api.albums(), widget.api.people(), widget.api.places()]);
      if (!mounted) return;
      setState(() {
        _albums = got[0];
        _people = got[1];
        _places = got[2];
        _error = null;
        _for = version;
      });
    } catch (e) {
      if (mounted) setState(() => _error = e);
    } finally {
      _loading = false;
    }
  }

  void _open(String title, List<Item> items) {
    Navigator.of(context).push(MaterialPageRoute(
      builder: (_) => Scaffold(
        appBar: AppBar(title: Text(title)),
        body: items.isEmpty
            ? const Center(child: Text('Nothing here.'))
            : PhotoGrid(items: items, api: widget.api, cache: widget.cache, library: widget.library),
      ),
    ));
  }

  Future<void> _openMembers(String kind, Map<String, dynamic> m, String title) async {
    try {
      final ids = await widget.api.members(kind, m['id'] as String);
      final lib = widget.library.byId;
      _open(title, [for (final id in ids) if (lib[id] != null) lib[id]!]);
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
  }

  Widget _cover(String? id, {BoxFit fit = BoxFit.cover}) {
    if (id == null) return const ColoredBox(color: Color(0xFF242527));
    return CachedNetworkImage(
      imageUrl: widget.api.thumbUrl(id),
      cacheKey: 't-$id',
      httpHeaders: widget.api.headers,
      cacheManager: widget.cache,
      fit: fit,
      placeholder: (_, _) => const ColoredBox(color: Color(0xFF242527)),
      errorWidget: (_, _, _) => const ColoredBox(color: Color(0xFF242527)),
    );
  }

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    return ListenableBuilder(
      listenable: widget.library,
      builder: (context, _) {
        final favorites = widget.library.items.where((i) => i.favorite).toList();
        final people = _people ?? [], albums = _albums ?? [], places = _places ?? [];
        return RefreshIndicator(
          onRefresh: _load,
          child: ListView(padding: const EdgeInsets.symmetric(vertical: 12), children: [
            ListTile(
              leading: ClipRRect(
                borderRadius: BorderRadius.circular(8),
                child: SizedBox(width: 56, height: 56, child: favorites.isEmpty ? const Icon(Icons.favorite_border) : _cover(favorites.first.id)),
              ),
              title: const Text('Favourites'),
              subtitle: Text('${favorites.length}'),
              onTap: () => _open('Favourites', favorites),
            ),
            if (_error != null && _albums == null)
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text("People and albums show when ${widget.api.pairing.name} is reachable.", style: t.bodyMedium?.copyWith(color: Colors.white70)),
              ),
            if (_albums == null && _error == null) const Padding(padding: EdgeInsets.all(24), child: Center(child: CircularProgressIndicator())),
            if (people.isNotEmpty) ...[
              Padding(padding: const EdgeInsets.fromLTRB(16, 20, 16, 10), child: Text('People', style: t.titleMedium)),
              SizedBox(
                height: 118,
                child: ListView.separated(
                  scrollDirection: Axis.horizontal,
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  itemCount: people.length,
                  separatorBuilder: (_, _) => const SizedBox(width: 14),
                  itemBuilder: (context, i) => _personTile(people[i], t),
                ),
              ),
            ],
            if (places.isNotEmpty) ...[
              Padding(padding: const EdgeInsets.fromLTRB(16, 20, 16, 10), child: Text('Places', style: t.titleMedium)),
              SizedBox(
                height: 150,
                child: ListView.separated(
                  scrollDirection: Axis.horizontal,
                  padding: const EdgeInsets.symmetric(horizontal: 16),
                  itemCount: places.length,
                  separatorBuilder: (_, _) => const SizedBox(width: 12),
                  itemBuilder: (context, i) => _placeTile(places[i], t),
                ),
              ),
            ],
            if (albums.isNotEmpty) ...[
              Padding(padding: const EdgeInsets.fromLTRB(16, 20, 16, 10), child: Text('Albums', style: t.titleMedium)),
              GridView.count(
                crossAxisCount: 2,
                shrinkWrap: true,
                physics: const NeverScrollableScrollPhysics(),
                padding: const EdgeInsets.symmetric(horizontal: 16),
                mainAxisSpacing: 14,
                crossAxisSpacing: 14,
                childAspectRatio: 0.82,
                children: [for (final a in albums) _albumTile(a, t)],
              ),
            ],
            if (_albums != null && albums.isEmpty && people.isEmpty && places.isEmpty)
              Padding(
                padding: const EdgeInsets.all(16),
                child: Text('Albums you make on the computer, the people it recognises and the places photos were taken show here.', style: t.bodyMedium?.copyWith(color: Colors.white70)),
              ),
          ]),
        );
      },
    );
  }

  Widget _personTile(Map<String, dynamic> p, TextTheme t) {
    final name = p['name'] as String?;
    return InkWell(
      onTap: () => _openMembers('person', p, name ?? 'Unnamed'),
      child: SizedBox(
        width: 76,
        child: Column(children: [
          ClipOval(
            child: SizedBox(
              width: 72,
              height: 72,
              child: CachedNetworkImage(
                imageUrl: widget.api.faceUrl(p['id'] as String),
                cacheKey: 'f-${p['id']}',
                httpHeaders: widget.api.headers,
                cacheManager: widget.cache,
                fit: BoxFit.cover,
                placeholder: (_, _) => const ColoredBox(color: Color(0xFF242527)),
                // No face picture yet: the person's newest photo.
                errorWidget: (_, _, _) => _cover(p['cover'] as String?),
              ),
            ),
          ),
          const SizedBox(height: 6),
          Text(name ?? 'Unnamed', style: t.bodySmall?.copyWith(color: name == null ? Colors.white54 : null), overflow: TextOverflow.ellipsis),
          Text('${p['count']}', style: t.labelSmall?.copyWith(color: Colors.white54)),
        ]),
      ),
    );
  }

  /// "Lahore" over its cover, with "Pakistan · 120" under it.
  Widget _placeTile(Map<String, dynamic> p, TextTheme t) {
    final name = p['name'] as String;
    final comma = name.indexOf(', ');
    final (town, country) = comma < 0 ? (name, '') : (name.substring(0, comma), name.substring(comma + 2));
    return InkWell(
      onTap: () => _openMembers('place', p, name),
      child: SizedBox(
        width: 104,
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          ClipRRect(borderRadius: BorderRadius.circular(10), child: SizedBox(width: 104, height: 104, child: _cover(p['cover'] as String?))),
          const SizedBox(height: 6),
          Text(town, style: t.bodySmall, overflow: TextOverflow.ellipsis),
          Text(country.isEmpty ? '${p['count']}' : '$country · ${p['count']}', style: t.labelSmall?.copyWith(color: Colors.white54), overflow: TextOverflow.ellipsis),
        ]),
      ),
    );
  }

  Widget _albumTile(Map<String, dynamic> a, TextTheme t) {
    return InkWell(
      onTap: () => _openMembers('album', a, a['name'] as String),
      child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
        Expanded(child: ClipRRect(borderRadius: BorderRadius.circular(10), child: SizedBox.expand(child: _cover(a['cover'] as String?)))),
        const SizedBox(height: 6),
        Text(a['name'] as String, style: t.bodyMedium, overflow: TextOverflow.ellipsis),
        Text('${a['count']}', style: t.labelSmall?.copyWith(color: Colors.white54)),
      ]),
    );
  }
}
