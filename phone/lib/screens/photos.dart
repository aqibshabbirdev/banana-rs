// The timeline: day headings and square thumbnails; tap one to open it, long-press to select
// several, pinch to change how many to a row, drag the scrubber on the right to fly through years.

import 'dart:async';
import 'dart:io';

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';
import 'package:intl/intl.dart';
import 'package:path_provider/path_provider.dart';

import '../api.dart';
import '../library.dart';
import 'viewer.dart';

/// Photos to a row, in every grid; pinching changes it, and it's kept.
final gridColumns = ValueNotifier<int>(4);
const _columnChoices = [2, 3, 4, 5, 7];

Future<File> _columnsFile() async => File('${(await getApplicationSupportDirectory()).path}/columns');

Future<void> loadGridColumns() async {
  try {
    final n = int.parse((await (await _columnsFile()).readAsString()).trim());
    if (_columnChoices.contains(n)) gridColumns.value = n;
  } catch (_) {}
}

void _setGridColumns(int n) {
  gridColumns.value = n;
  _columnsFile().then((f) => f.writeAsString('$n')).ignore();
}

/// Height of a day heading row (fixed, so the scrubber can tell where everything is).
const _dayRowHeight = 48.0;

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

  final _scroll = ScrollController();

  /// The scrubber shows while scrolling and for a moment after.
  bool _scrubShown = false, _scrubbing = false;
  Timer? _scrubHide;

  /// Two fingers on the grid: how far apart they started, and whether this pinch already
  /// changed the columns.
  final Map<int, Offset> _pointers = {};
  double? _pinchStart;
  bool _pinched = false;

  /// The current layout, for the scrubber and to keep the same photos on screen across a pinch.
  List<TimelineRow> _rows = [];
  List<double> _tops = [0];
  List<Item> _laidOut = [];

  @override
  void dispose() {
    _scroll.dispose();
    _scrubHide?.cancel();
    super.dispose();
  }

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

  /// Row heights at this width: day headings, then square photos with a 2-pixel gap.
  void _layout(List<Item> items, int columns, double side) {
    _laidOut = items;
    _rows = widget.byDay ? layoutRows(items, columns) : [for (var s = 0; s < items.length; s += columns) PhotosRow(s, (s + columns).clamp(0, items.length))];
    final tops = List<double>.filled(_rows.length + 1, 0);
    for (var i = 0; i < _rows.length; i++) {
      tops[i + 1] = tops[i] + (_rows[i] is DayRow ? _dayRowHeight : side + 2);
    }
    _tops = tops;
  }

  /// The row at a scroll offset.
  int _rowAt(double offset) {
    var lo = 0, hi = _rows.length - 1;
    while (lo < hi) {
      final mid = (lo + hi + 1) ~/ 2;
      _tops[mid] <= offset ? lo = mid : hi = mid - 1;
    }
    return lo;
  }

  /// The first photo in or after a row.
  int _itemAt(int row) {
    for (var r = row; r < _rows.length; r++) {
      if (_rows[r] case PhotosRow p) return p.start;
    }
    return _laidOut.length - 1;
  }

  void _onPointerDown(PointerDownEvent e) {
    _pointers[e.pointer] = e.position;
    if (_pointers.length == 2) {
      final p = _pointers.values.toList();
      _pinchStart = (p[0] - p[1]).distance;
      _pinched = false;
    }
  }

  void _onPointerMove(PointerMoveEvent e) {
    if (!_pointers.containsKey(e.pointer)) return;
    _pointers[e.pointer] = e.position;
    final start = _pinchStart;
    if (_pointers.length != 2 || start == null || start == 0 || _pinched) return;
    final p = _pointers.values.toList();
    final ratio = (p[0] - p[1]).distance / start;
    final i = _columnChoices.indexOf(gridColumns.value);
    // Spreading the fingers zooms in (fewer, bigger photos); pinching zooms out.
    final next = ratio > 1.3 ? i - 1 : (ratio < 0.77 ? i + 1 : i);
    if (next != i && next >= 0 && next < _columnChoices.length) {
      _pinched = true;
      _keepPlaceAcross(() => _setGridColumns(_columnChoices[next]));
    }
  }

  void _onPointerUp(PointerEvent e) {
    _pointers.remove(e.pointer);
    if (_pointers.length < 2) _pinchStart = null;
  }

  /// Changes the layout, then scrolls so the photo that was at the top still is.
  void _keepPlaceAcross(VoidCallback change) {
    final top = _scroll.hasClients && _rows.isNotEmpty ? _itemAt(_rowAt(_scroll.offset)) : 0;
    change();
    WidgetsBinding.instance.addPostFrameCallback((_) {
      if (!_scroll.hasClients) return;
      for (var r = 0; r < _rows.length; r++) {
        if (_rows[r] case PhotosRow p when p.start <= top && top < p.end) {
          // Its day heading too, when it starts the day.
          final row = r > 0 && _rows[r - 1] is DayRow ? r - 1 : r;
          _scroll.jumpTo(_tops[row].clamp(0, _scroll.position.maxScrollExtent));
          return;
        }
      }
    });
  }

  bool _onScroll(ScrollNotification n) {
    if (n.metrics.axis != Axis.vertical || n.depth != 0) return false;
    if (n is ScrollUpdateNotification && !_scrubShown && n.metrics.maxScrollExtent > n.metrics.viewportDimension * 4) {
      setState(() => _scrubShown = true);
    }
    if (n is ScrollUpdateNotification || n is ScrollEndNotification) {
      _scrubHide?.cancel();
      _scrubHide = Timer(const Duration(milliseconds: 1500), () {
        if (mounted && !_scrubbing) setState(() => _scrubShown = false);
      });
    }
    return false;
  }

  void _scrubTo(double y, double height) {
    if (!_scroll.hasClients) return;
    final max = _scroll.position.maxScrollExtent;
    _scroll.jumpTo(((y - _scrubThumb / 2) / (height - _scrubThumb)).clamp(0.0, 1.0) * max);
  }

  static const _scrubThumb = 48.0;

  /// The scrubber: a handle on the right edge, with the month it's at while dragging.
  Widget _scrubber(double height) {
    if (!_scrubShown || !_scroll.hasClients || _rows.isEmpty) return const SizedBox();
    final max = _scroll.position.maxScrollExtent;
    final f = max <= 0 ? 0.0 : (_scroll.offset / max).clamp(0.0, 1.0);
    final top = f * (height - _scrubThumb);
    final item = _laidOut.isEmpty ? null : _laidOut[_itemAt(_rowAt(_scroll.offset)).clamp(0, _laidOut.length - 1)];
    final scheme = Theme.of(context).colorScheme;
    return Positioned(
      right: 0,
      top: 0,
      bottom: 0,
      width: _scrubbing ? 220 : 44,
      child: GestureDetector(
        behavior: HitTestBehavior.translucent,
        onVerticalDragStart: (d) {
          _scrubHide?.cancel();
          setState(() => _scrubbing = true);
        },
        onVerticalDragUpdate: (d) => _scrubTo(d.localPosition.dy, height),
        onVerticalDragEnd: (_) {
          setState(() => _scrubbing = false);
          _scrubHide = Timer(const Duration(milliseconds: 1500), () => mounted ? setState(() => _scrubShown = false) : null);
        },
        child: Stack(children: [
          Positioned(
            right: 4,
            top: top,
            child: Row(children: [
              if (_scrubbing && item != null)
                Container(
                  margin: const EdgeInsets.only(right: 10),
                  padding: const EdgeInsets.symmetric(horizontal: 14, vertical: 8),
                  decoration: BoxDecoration(color: scheme.primaryContainer, borderRadius: BorderRadius.circular(20)),
                  child: Text(DateFormat('MMM y').format(item.taken), style: TextStyle(color: scheme.onPrimaryContainer, fontWeight: FontWeight.w600)),
                ),
              Container(
                width: 34,
                height: _scrubThumb,
                decoration: BoxDecoration(
                  color: _scrubbing ? scheme.primary : scheme.surfaceContainerHighest,
                  borderRadius: BorderRadius.circular(17),
                  boxShadow: const [BoxShadow(blurRadius: 4, color: Colors.black45)],
                ),
                child: Icon(Icons.unfold_more, size: 20, color: _scrubbing ? scheme.onPrimary : scheme.onSurface),
              ),
            ]),
          ),
        ]),
      ),
    );
  }

  @override
  Widget build(BuildContext context) {
    final items = _items;
    final now = DateTime.now();
    final selecting = _selected.isNotEmpty;
    final grid = ValueListenableBuilder<int>(
      valueListenable: gridColumns,
      builder: (context, columns, _) => LayoutBuilder(builder: (context, box) {
        final side = (box.maxWidth - (columns - 1) * 2) / columns;
        _layout(items, columns, side);
        final rows = _rows;
        final list = ListView.builder(
          controller: _scroll,
          itemCount: rows.length,
          itemExtentBuilder: (i, _) => i < rows.length && rows[i] is DayRow ? _dayRowHeight : side + 2,
          // Room for the selection bar under the last row.
          padding: EdgeInsets.only(bottom: selecting ? 88 : 0),
          itemBuilder: (context, i) => switch (rows[i]) {
            DayRow r => Padding(
                padding: const EdgeInsets.fromLTRB(14, 18, 14, 8),
                child: Text(dayLabel(r.day, now), style: Theme.of(context).textTheme.titleSmall, maxLines: 1),
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
                        small: columns >= 5,
                        onTap: () => selecting ? _toggle(items[k]) : _open(context, items, k),
                        onLongPress: () => _toggle(items[k]),
                      ),
                    ),
                  ],
                ]),
              ),
          },
        );
        return Listener(
          onPointerDown: _onPointerDown,
          onPointerMove: _onPointerMove,
          onPointerUp: _onPointerUp,
          onPointerCancel: _onPointerUp,
          child: NotificationListener<ScrollNotification>(
            onNotification: _onScroll,
            // Only the scrubber follows the scroll position (not the whole grid).
            child: Stack(children: [
              list,
              Positioned.fill(child: ListenableBuilder(listenable: _scroll, builder: (context, _) => Stack(children: [_scrubber(box.maxHeight)]))),
            ]),
          ),
        );
      }),
    );
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
  const Thumb({super.key, required this.item, required this.api, required this.cache, required this.onTap, this.onLongPress, this.selected, this.small = false});

  final Item item;
  final Api api;
  final CacheManager cache;
  final VoidCallback onTap;
  final VoidCallback? onLongPress;

  /// While selecting: whether this one is in the selection (null when not selecting).
  final bool? selected;

  /// Many to a row: no room for a video's length.
  final bool small;

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
              if (item.duration != null && !small)
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
