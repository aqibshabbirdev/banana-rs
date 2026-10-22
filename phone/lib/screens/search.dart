// Search on the computer: by name or date, else by what's in the photos (the computer's AI).

import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';

import '../api.dart';
import '../library.dart';
import 'photos.dart';

class SearchPage extends StatefulWidget {
  const SearchPage({super.key, required this.library, required this.api, required this.cache});

  final Library library;
  final Api api;
  final CacheManager cache;

  @override
  State<SearchPage> createState() => _SearchPageState();
}

class _SearchPageState extends State<SearchPage> {
  final _query = TextEditingController();
  List<Item>? _results;
  String _kind = '';
  bool _busy = false;
  Object? _error;

  Future<void> _search(String q) async {
    if (q.trim().isEmpty) return;
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final r = await widget.api.search(q.trim());
      final lib = widget.library.byId;
      setState(() {
        _results = [for (final id in r.ids) if (lib[id] != null) lib[id]!];
        _kind = r.kind;
      });
    } catch (e) {
      setState(() => _error = e);
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final results = _results;
    return Column(children: [
      Padding(
        padding: const EdgeInsets.fromLTRB(12, 8, 12, 4),
        child: SearchBar(
          controller: _query,
          hintText: 'beach, birthday cake, 2024, october…',
          leading: const Icon(Icons.search),
          trailing: [if (_busy) const SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2))],
          onSubmitted: _search,
        ),
      ),
      if (_error != null) Padding(padding: const EdgeInsets.all(16), child: Text('$_error')),
      if (results != null && _error == null)
        Padding(
          padding: const EdgeInsets.all(8),
          child: Text(
            results.isEmpty
                ? 'Nothing found.'
                : '${results.length} ${_kind == 'content' ? 'photos that look like “${_query.text.trim()}”, best first' : 'matching photos'}',
            style: Theme.of(context).textTheme.bodySmall,
          ),
        ),
      Expanded(
        child: results == null || results.isEmpty
            ? const SizedBox()
            : PhotoGrid(items: results, api: widget.api, cache: widget.cache, library: widget.library, byDay: _kind != 'content'),
      ),
    ]);
  }
}
