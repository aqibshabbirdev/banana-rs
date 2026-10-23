// The timeline: day headings and square thumbnails, four to a row; tap one to open it, or
// long-press to select several.

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';
import 'package:intl/intl.dart';

import '../api.dart';
import '../library.dart';
import 'viewer.dart';

const columns = 4;

class PhotosPage extends StatelessWidget {
  const PhotosPage({super.key, required this.library, required this.api, required this.cache});

  final Library library;
  final Api api;
  final CacheManager cache;

  @override
  Widget build(BuildContext context) {
    return ListenableBuilder(
      listenable: library,
      builder: (context, _) {
        if (library.items.isEmpty) {
          return Center(
            child: library.loading
                ? const CircularProgressIndicator()
                : Text(library.error != null ? '${library.error}' : 'No photos on the computer yet.', textAlign: TextAlign.center),
          );
        }
        final grid = RefreshIndicator(
          onRefresh: () => library.refresh(force: true),
          child: PhotoGrid(items: library.items, api: api, cache: cache, library: library),
        );
        if (library.error == null) return grid;
        // The computer stopped answering: say so, and keep showing what was loaded.
        return Column(children: [
          Material(
            color: Theme.of(context).colorScheme.errorContainer,
            child: Padding(
              padding: const EdgeInsets.fromLTRB(16, 6, 8, 6),
              child: Row(children: [
                const Icon(Icons.wifi_off, size: 18),
                const SizedBox(width: 10),
                Expanded(child: Text("Can't reach ${api.pairing.name} · showing what was loaded", style: Theme.of(context).textTheme.bodySmall)),
                TextButton(onPressed: library.loading ? null : () => library.refresh(), child: const Text('Retry')),
              ]),
            ),
          ),
          Expanded(child: grid),
        ]);
      },
    );
  }
}

/// Day-grouped grid of `items`, lazily built row by row (fine for tens of thousands). Long-press
/// a photo to select; then tap to add or remove, and favourite or trash the selection.
class PhotoGrid extends StatefulWidget {
  const PhotoGrid({super.key, required this.items, required this.api, required this.cache, required this.library, this.byDay = true});

  final List<Item> items;
  final Api api;
  final CacheManager cache;
  final Library library;

  /// Search results are best first, so they aren't split into days.
  final bool byDay;

  @override
  State<PhotoGrid> createState() => _PhotoGridState();
}

class _PhotoGridState extends State<PhotoGrid> {
  final Set<String> _selected = {};

  /// Trashed from here: hidden at once (lists passed in, like search results, don't update).
  final Set<String> _gone = {};
  bool _busy = false;

  List<Item> get _items => _gone.isEmpty ? widget.items : widget.items.where((i) => !_gone.contains(i.id)).toList();

  void _toggle(Item item) => setState(() => _selected.contains(item.id) ? _selected.remove(item.id) : _selected.add(item.id));

  List<Item> _chosen() => [for (final i in widget.items) if (_selected.contains(i.id)) i];

  void _say(String text) => ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));

  Future<void> _favorite() async {
    final chosen = _chosen();
    // All favourites already: the button takes them off.
    final on = !chosen.every((i) => i.favorite);
    setState(() => _busy = true);
    try {
      await widget.library.setFavorites(chosen, on);
      setState(_selected.clear);
    } catch (e) {
      _say('$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _trash() async {
    final chosen = _chosen();
    final n = chosen.length, what = n == 1 ? 'this item' : 'these $n items';
    final ok = await showDialog<bool>(
      context: context,
      builder: (c) => AlertDialog(
        title: Text('Move $what to the Trash?'),
        content: Text(n == 1
            ? "It goes to the Trash on ${widget.api.pairing.name}, where it can be put back."
            : "They go to the Trash on ${widget.api.pairing.name}, where they can be put back."),
        actions: [
          TextButton(onPressed: () => Navigator.pop(c, false), child: const Text('Cancel')),
          TextButton(onPressed: () => Navigator.pop(c, true), child: const Text('Move to Trash')),
        ],
      ),
    );
    if (ok != true) return;
    setState(() => _busy = true);
    try {
      final r = await widget.library.trash(chosen);
      final still = widget.library.byId;
      setState(() {
        _gone.addAll([for (final i in chosen) if (!still.containsKey(i.id)) i.id]);
        _selected.clear();
      });
      var text = '${r.trashed} moved to the Trash on ${widget.api.pairing.name}.';
      if (r.skipped > 0) text += ' ${r.skipped} in an Apple Photos library left alone; delete those in Photos.';
      if (r.errors.isNotEmpty) text += " ${r.errors.length} couldn't be moved: ${r.errors.first}";
      _say(text);
    } catch (e) {
      _say('$e');
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    final items = _items;
    final List<TimelineRow> rows = widget.byDay ? layoutRows(items, columns) : [for (var s = 0; s < items.length; s += columns) PhotosRow(s, (s + columns).clamp(0, items.length))];
    final now = DateTime.now();
    final selecting = _selected.isNotEmpty;
    final grid = LayoutBuilder(builder: (context, box) {
      final side = (box.maxWidth - (columns - 1) * 2) / columns;
      return ListView.builder(
        itemCount: rows.length,
        // Room for the selection bar under the last row.
        padding: EdgeInsets.only(bottom: selecting ? 88 : 0),
        itemBuilder: (context, i) => switch (rows[i]) {
          DayRow r => Padding(
              padding: const EdgeInsets.fromLTRB(14, 18, 14, 8),
              child: Text(dayLabel(r.day, now), style: Theme.of(context).textTheme.titleSmall),
            ),
          PhotosRow r => Padding(
              padding: const EdgeInsets.only(bottom: 2),
              child: Row(children: [
                for (var k = r.start; k < r.end; k++) ...[
                  if (k > r.start) const SizedBox(width: 2),
                  SizedBox(
                    width: side,
                    height: side,
                    child: Thumb(
                      item: items[k],
                      api: widget.api,
                      cache: widget.cache,
                      selected: selecting ? _selected.contains(items[k].id) : null,
                      onTap: () => selecting ? _toggle(items[k]) : _open(context, items, k),
                      onLongPress: () => _toggle(items[k]),
                    ),
                  ),
                ],
              ]),
            ),
        },
      );
    });
    if (!selecting) return grid;
    final allFavorite = _chosen().every((i) => i.favorite);
    return PopScope(
      // Back leaves selecting first.
      canPop: false,
      onPopInvokedWithResult: (_, _) => setState(_selected.clear),
      child: Stack(children: [
        grid,
        Positioned(
          left: 12,
          right: 12,
          bottom: 12,
          child: Material(
            color: Theme.of(context).colorScheme.surfaceContainerHighest,
            borderRadius: BorderRadius.circular(16),
            elevation: 6,
            child: Padding(
              padding: const EdgeInsets.symmetric(horizontal: 4, vertical: 4),
              child: Row(children: [
                IconButton(tooltip: 'Done', onPressed: () => setState(_selected.clear), icon: const Icon(Icons.close)),
                Expanded(child: Text('${_selected.length} selected', style: Theme.of(context).textTheme.titleSmall)),
                if (_busy) const Padding(padding: EdgeInsets.all(12), child: SizedBox(width: 20, height: 20, child: CircularProgressIndicator(strokeWidth: 2))),
                IconButton(
                  tooltip: allFavorite ? 'Unfavorite' : 'Favorite',
                  onPressed: _busy ? null : _favorite,
                  icon: Icon(allFavorite ? Icons.favorite : Icons.favorite_border),
                ),
                IconButton(tooltip: 'Move to Trash', onPressed: _busy ? null : _trash, icon: const Icon(Icons.delete_outline)),
              ]),
            ),
          ),
        ),
      ]),
    );
  }

  void _open(BuildContext context, List<Item> items, int index) {
    Navigator.of(context).push(MaterialPageRoute(builder: (_) => Viewer(items: items, index: index, api: widget.api, cache: widget.cache, library: widget.library)));
  }
}

class Thumb extends StatelessWidget {
  const Thumb({super.key, required this.item, required this.api, required this.cache, required this.onTap, this.onLongPress, this.selected});

  final Item item;
  final Api api;
  final CacheManager cache;
  final VoidCallback onTap;
  final VoidCallback? onLongPress;

  /// While selecting: whether this one is in the selection (null when not selecting).
  final bool? selected;

  @override
  Widget build(BuildContext context) {
    final on = selected == true;
    return GestureDetector(
      onTap: onTap,
      onLongPress: onLongPress,
      child: Stack(fit: StackFit.expand, children: [
        AnimatedPadding(
          duration: const Duration(milliseconds: 120),
          padding: EdgeInsets.all(on ? 8 : 0),
          child: CachedNetworkImage(
            imageUrl: api.thumbUrl(item.id),
            cacheKey: 't-${item.id}',
            httpHeaders: api.headers,
            cacheManager: cache,
            fit: BoxFit.cover,
            fadeInDuration: const Duration(milliseconds: 120),
            placeholder: (_, _) => const ColoredBox(color: Color(0xFF242527)),
            errorWidget: (_, _, _) => const ColoredBox(color: Color(0xFF242527), child: Icon(Icons.broken_image_outlined, color: Colors.white38)),
          ),
        ),
        if (item.video)
          Positioned(
            right: 4,
            top: 4,
            child: Row(children: [
              if (item.duration != null)
                Text(formatDuration(item.duration!), style: const TextStyle(fontSize: 11, shadows: [Shadow(blurRadius: 3)])),
              const Icon(Icons.play_circle_fill, size: 16, shadows: [Shadow(blurRadius: 3)]),
            ]),
          ),
        if (item.favorite) const Positioned(left: 4, bottom: 4, child: Icon(Icons.favorite, size: 14, shadows: [Shadow(blurRadius: 3)])),
        if (selected != null)
          Positioned(
            left: 4,
            top: 4,
            child: Icon(
              on ? Icons.check_circle : Icons.radio_button_unchecked,
              size: 22,
              color: on ? Theme.of(context).colorScheme.primary : Colors.white70,
              shadows: const [Shadow(blurRadius: 3)],
            ),
          ),
      ]),
    );
  }
}

/// "Today", "Yesterday", "Wed, Oct 21", or "Oct 21, 2024" for other years.
String dayLabel(DateTime day, DateTime now) {
  final today = DateTime.utc(now.year, now.month, now.day);
  final diff = today.difference(day).inDays;
  if (diff == 0) return 'Today';
  if (diff == 1) return 'Yesterday';
  return day.year == now.year ? DateFormat('EEE, MMM d').format(day) : DateFormat('EEE, MMM d, y').format(day);
}

String formatDuration(double secs) {
  final s = secs.round();
  final h = s ~/ 3600, m = (s % 3600) ~/ 60, r = s % 60;
  String two(int n) => n.toString().padLeft(2, '0');
  return h > 0 ? '$h:${two(m)}:${two(r)}' : '$m:${two(r)}';
}
