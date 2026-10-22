// One photo or video at a time: swipe between them, pinch to zoom, play videos, favourite.

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';
import 'package:intl/intl.dart';
import 'package:video_player/video_player.dart';

import '../api.dart';
import '../library.dart';
import 'photos.dart' show formatDuration;

class Viewer extends StatefulWidget {
  const Viewer({super.key, required this.items, required this.index, required this.api, required this.cache, required this.library});

  final List<Item> items;
  final int index;
  final Api api;
  final CacheManager cache;
  final Library library;

  @override
  State<Viewer> createState() => _ViewerState();
}

class _ViewerState extends State<Viewer> {
  late final PageController _pages = PageController(initialPage: widget.index);
  late int _index = widget.index;
  bool _chrome = true;

  Item get _item => widget.items[_index];

  Future<void> _toggleFavorite() async {
    try {
      await widget.library.setFavorite(_item, !_item.favorite);
      setState(() {});
    } catch (e) {
      if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text('$e')));
    }
  }

  @override
  Widget build(BuildContext context) {
    final item = _item;
    return Scaffold(
      backgroundColor: Colors.black,
      extendBodyBehindAppBar: true,
      appBar: _chrome
          ? AppBar(
              backgroundColor: Colors.black54,
              title: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
                Text(DateFormat('EEE, MMM d, y').format(item.taken), style: const TextStyle(fontSize: 16)),
                Text(DateFormat('HH:mm').format(item.taken), style: const TextStyle(fontSize: 12, color: Colors.white60)),
              ]),
              actions: [
                IconButton(
                  tooltip: item.favorite ? 'Unfavorite' : 'Favorite',
                  onPressed: _toggleFavorite,
                  icon: Icon(item.favorite ? Icons.favorite : Icons.favorite_border),
                ),
              ],
            )
          : null,
      body: PageView.builder(
        controller: _pages,
        itemCount: widget.items.length,
        onPageChanged: (i) => setState(() => _index = i),
        itemBuilder: (context, i) {
          final it = widget.items[i];
          return GestureDetector(
            onTap: () => setState(() => _chrome = !_chrome),
            child: it.video ? VideoPage(item: it, api: widget.api, cache: widget.cache, active: i == _index) : PhotoPage(item: it, api: widget.api, cache: widget.cache),
          );
        },
      ),
    );
  }
}

/// The photo at screen size, zoomable; the thumbnail shows while it loads.
class PhotoPage extends StatelessWidget {
  const PhotoPage({super.key, required this.item, required this.api, required this.cache});

  final Item item;
  final Api api;
  final CacheManager cache;

  @override
  Widget build(BuildContext context) {
    final px = (MediaQuery.sizeOf(context).longestSide * MediaQuery.devicePixelRatioOf(context)).round().clamp(1024, 4096);
    Widget thumb() => CachedNetworkImage(imageUrl: api.thumbUrl(item.id), cacheKey: 't-${item.id}', httpHeaders: api.headers, cacheManager: cache, fit: BoxFit.contain);
    return InteractiveViewer(
      maxScale: 6,
      child: Center(
        child: CachedNetworkImage(
          imageUrl: api.photoUrl(item.id, max: px),
          cacheKey: 'p$px-${item.id}',
          httpHeaders: api.headers,
          cacheManager: cache,
          fit: BoxFit.contain,
          fadeInDuration: Duration.zero,
          placeholder: (_, _) => Stack(alignment: Alignment.center, children: [thumb(), const CircularProgressIndicator()]),
          errorWidget: (_, _, e) => Center(child: Text("Couldn't load this photo.\n$e", textAlign: TextAlign.center)),
        ),
      ),
    );
  }
}

/// Plays a video from the computer through a signed link (native players can't be pinned).
class VideoPage extends StatefulWidget {
  const VideoPage({super.key, required this.item, required this.api, required this.cache, required this.active});

  final Item item;
  final Api api;
  final CacheManager cache;

  /// The page on screen; others don't start loading.
  final bool active;

  @override
  State<VideoPage> createState() => _VideoPageState();
}

class _VideoPageState extends State<VideoPage> {
  VideoPlayerController? _video;
  Object? _error;

  @override
  void initState() {
    super.initState();
    if (widget.active) _start();
  }

  @override
  void didUpdateWidget(VideoPage old) {
    super.didUpdateWidget(old);
    if (widget.active && _video == null && _error == null) _start();
    if (!widget.active) _video?.pause();
  }

  Future<void> _start() async {
    try {
      final url = await widget.api.mediaUrl(widget.item.id);
      final v = VideoPlayerController.networkUrl(Uri.parse(url));
      _video = v;
      await v.initialize();
      v.addListener(() => mounted ? setState(() {}) : null);
      if (widget.active) await v.play();
      if (mounted) setState(() {});
    } catch (e) {
      if (mounted) setState(() => _error = e);
    }
  }

  @override
  void dispose() {
    _video?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final v = _video;
    if (_error != null) return Center(child: Text("Couldn't play this video.\n$_error", textAlign: TextAlign.center));
    if (v == null || !v.value.isInitialized) {
      return Stack(alignment: Alignment.center, children: [
        CachedNetworkImage(imageUrl: widget.api.thumbUrl(widget.item.id), cacheKey: 't-${widget.item.id}', httpHeaders: widget.api.headers, cacheManager: widget.cache),
        const CircularProgressIndicator(),
      ]);
    }
    final pos = v.value.position, len = v.value.duration;
    return Stack(children: [
      Center(child: AspectRatio(aspectRatio: v.value.aspectRatio, child: VideoPlayer(v))),
      Positioned(
        left: 0,
        right: 0,
        bottom: 0,
        child: SafeArea(
          child: Container(
            color: Colors.black45,
            padding: const EdgeInsets.symmetric(horizontal: 8),
            child: Row(children: [
              IconButton(
                onPressed: () => v.value.isPlaying ? v.pause() : v.play(),
                icon: Icon(v.value.isPlaying ? Icons.pause : Icons.play_arrow),
              ),
              Text(formatDuration(pos.inMilliseconds / 1000), style: const TextStyle(fontSize: 12)),
              Expanded(child: VideoProgressIndicator(v, allowScrubbing: true, padding: const EdgeInsets.symmetric(horizontal: 10))),
              Text(formatDuration(len.inMilliseconds / 1000), style: const TextStyle(fontSize: 12)),
            ]),
          ),
        ),
      ),
    ]);
  }
}
