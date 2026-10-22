// The timeline: day headings and square thumbnails, four to a row; tap one to open it.

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
        return RefreshIndicator(
          onRefresh: () => library.refresh(force: true),
          child: PhotoGrid(items: library.items, api: api, cache: cache, library: library),
        );
      },
    );
  }
}

/// Day-grouped grid of `items`, lazily built row by row (fine for tens of thousands).
class PhotoGrid extends StatelessWidget {
  const PhotoGrid({super.key, required this.items, required this.api, required this.cache, required this.library, this.byDay = true});

  final List<Item> items;
  final Api api;
  final CacheManager cache;
  final Library library;

  /// Search results are best first, so they aren't split into days.
  final bool byDay;

  @override
  Widget build(BuildContext context) {
    final List<TimelineRow> rows = byDay ? layoutRows(items, columns) : [for (var s = 0; s < items.length; s += columns) PhotosRow(s, (s + columns).clamp(0, items.length))];
    final now = DateTime.now();
    return LayoutBuilder(builder: (context, box) {
      final side = (box.maxWidth - (columns - 1) * 2) / columns;
      return ListView.builder(
        itemCount: rows.length,
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
                  SizedBox(width: side, height: side, child: Thumb(item: items[k], api: api, cache: cache, onTap: () => _open(context, k))),
                ],
              ]),
            ),
        },
      );
    });
  }

  void _open(BuildContext context, int index) {
    Navigator.of(context).push(MaterialPageRoute(builder: (_) => Viewer(items: items, index: index, api: api, cache: cache, library: library)));
  }
}

class Thumb extends StatelessWidget {
  const Thumb({super.key, required this.item, required this.api, required this.cache, required this.onTap});

  final Item item;
  final Api api;
  final CacheManager cache;
  final VoidCallback onTap;

  @override
  Widget build(BuildContext context) {
    return GestureDetector(
      onTap: onTap,
      child: Stack(fit: StackFit.expand, children: [
        CachedNetworkImage(
          imageUrl: api.thumbUrl(item.id),
          cacheKey: 't-${item.id}',
          httpHeaders: api.headers,
          cacheManager: cache,
          fit: BoxFit.cover,
          fadeInDuration: const Duration(milliseconds: 120),
          placeholder: (_, _) => const ColoredBox(color: Color(0xFF242527)),
          errorWidget: (_, _, _) => const ColoredBox(color: Color(0xFF242527), child: Icon(Icons.broken_image_outlined, color: Colors.white38)),
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
