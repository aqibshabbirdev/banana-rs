// One photo or video at a time: swipe between them, pinch to zoom, play videos, favourite,
// save to this phone or share.

import 'dart:async';
import 'dart:io';

import 'package:cached_network_image/cached_network_image.dart';
import 'package:flutter/material.dart';
import 'package:flutter_cache_manager/flutter_cache_manager.dart';
import 'package:intl/intl.dart';
import 'package:path_provider/path_provider.dart';
import 'package:photo_manager/photo_manager.dart';
import 'package:share_plus/share_plus.dart';
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

  /// Saving or sharing: what (for the progress bar), and how far the download got (0–1, or null
  /// while its size isn't known).
  String? _busy;
  double? _progress;

  void _say(String text) {
    if (mounted) ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
  }

  /// The file from the computer in this phone's temporary folder (kept for an hour, so saving
  /// and then sharing the same one fetches it once).
  Future<File> _fetch(Item item, {required bool original}) async {
    final dir = Directory('${(await getTemporaryDirectory()).path}/from-computer');
    await dir.create(recursive: true);
    final old = DateTime.now().subtract(const Duration(hours: 1));
    for (final f in dir.listSync().whereType<File>()) {
      if (f.statSync().modified.isBefore(old)) f.deleteSync();
    }
    final name = original ? item.name : '${item.name.replaceAll(RegExp(r'\.[^.]*$'), '')}.jpg';
    final f = File('${dir.path}/${item.id}-$name');
    if (f.existsSync() && f.lengthSync() > 0) return f;
    final part = File('${f.path}.part');
    await widget.api.download(item.id, part, original: original, progress: (got, total) {
      if (mounted) setState(() => _progress = total > 0 ? got / total : null);
    });
    return part.rename(f.path);
  }

  Future<void> _run(String what, Future<void> Function(Item) job) async {
    if (_busy != null) return;
    setState(() {
      _busy = what;
      _progress = null;
    });
    try {
      await job(_item);
    } catch (e) {
      _say("Couldn't get it from ${widget.api.pairing.name}: $e");
    } finally {
      if (mounted) setState(() => _busy = null);
    }
  }

  /// Saves the original to this phone's gallery. The same file, so a backup skips it later.
  Future<void> _save(Item item) async {
    final p = await PhotoManager.requestPermissionExtend(requestOption: const PermissionRequestOption(iosAccessLevel: IosAccessLevel.addOnly));
    if (!p.hasAccess) return _say('Allow Banana Photos to add to your photos (in Settings) to save it here.');
    final f = await _fetch(item, original: true);
    // Its date as a local time, so the gallery files it on the day it was taken.
    final t = item.taken, taken = DateTime(t.year, t.month, t.day, t.hour, t.minute, t.second);
    if (item.video) {
      await PhotoManager.editor.saveVideo(f, title: item.name, creationDate: taken);
    } else {
      await PhotoManager.editor.saveImageWithPath(f.path, title: item.name, creationDate: taken);
    }
    _say(item.video ? 'Video saved to this phone.' : 'Photo saved to this phone.');
  }

  /// Not every app opens HEIC or RAW: those are shared as JPEG.
  static final _convertForSharing = RegExp(r'\.(heic|heif|hif|avif|dng|cr2|cr3|nef|arw|raf|orf|rw2|tif|tiff)$', caseSensitive: false);

  Future<void> _share(Item item) async {
    final f = await _fetch(item, original: item.video || !_convertForSharing.hasMatch(item.name));
    if (!mounted) return;
    final box = context.findRenderObject() as RenderBox?;
    await SharePlus.instance.share(ShareParams(
      files: [XFile(f.path)],
      // Where the share sheet points from on iPads.
      sharePositionOrigin: box == null ? null : box.localToGlobal(Offset.zero) & box.size,
    ));
  }

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
              bottom: _busy == null
                  ? null
                  : PreferredSize(preferredSize: const Size.fromHeight(2), child: LinearProgressIndicator(value: _progress, minHeight: 2)),
              actions: [
                IconButton(
                  tooltip: 'Share',
                  onPressed: _busy == null ? () => _run('share', _share) : null,
                  icon: const Icon(Icons.share_outlined),
                ),
                IconButton(
                  tooltip: 'Save to this phone',
                  onPressed: _busy == null ? () => _run('save', _save) : null,
                  icon: const Icon(Icons.download_outlined),
                ),
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

/// The photo at screen size, zoomable. The thumbnail shows, with a spinner, while it loads and
/// while the computer can't be reached; then it tries again by itself every few seconds.
class PhotoPage extends StatefulWidget {
  const PhotoPage({super.key, required this.item, required this.api, required this.cache});

  final Item item;
  final Api api;
  final CacheManager cache;

  @override
  State<PhotoPage> createState() => _PhotoPageState();
}

/// How long to wait before asking the computer again for something that didn't load.
const retryAfter = Duration(seconds: 4);

class _PhotoPageState extends State<PhotoPage> {
  int _attempt = 0;
  Timer? _retry;

  @override
  void dispose() {
    _retry?.cancel();
    super.dispose();
  }

  void _retryLater(String url, String key) {
    _retry ??= Timer(retryAfter, () async {
      await CachedNetworkImage.evictFromCache(url, cacheKey: key, cacheManager: widget.cache);
      if (mounted) {
        setState(() {
          _retry = null;
          _attempt++;
        });
      }
    });
  }

  @override
  Widget build(BuildContext context) {
    final item = widget.item, api = widget.api;
    final px = (MediaQuery.sizeOf(context).longestSide * MediaQuery.devicePixelRatioOf(context)).round().clamp(1024, 4096);
    final url = api.photoUrl(item.id, max: px), key = 'p$px-${item.id}';
    Widget waiting() => ThumbWithSpinner(item: item, api: api, cache: widget.cache);
    return InteractiveViewer(
      maxScale: 6,
      child: Center(
        child: CachedNetworkImage(
          key: ValueKey('$key#$_attempt'),
          imageUrl: url,
          cacheKey: key,
          httpHeaders: api.headers,
          cacheManager: widget.cache,
          fit: BoxFit.contain,
          fadeInDuration: Duration.zero,
          placeholder: (_, _) => waiting(),
          errorWidget: (_, _, _) {
            _retryLater(url, key);
            return waiting();
          },
        ),
      ),
    );
  }
}

/// The cached thumbnail, full screen, with a spinner on top.
class ThumbWithSpinner extends StatelessWidget {
  const ThumbWithSpinner({super.key, required this.item, required this.api, required this.cache});

  final Item item;
  final Api api;
  final CacheManager cache;

  @override
  Widget build(BuildContext context) {
    // Fills the page (it may sit in a Center, which would shrink it to the spinner).
    return SizedBox.expand(
      child: Stack(fit: StackFit.expand, children: [
        CachedNetworkImage(
          imageUrl: api.thumbUrl(item.id),
          cacheKey: 't-${item.id}',
          httpHeaders: api.headers,
          cacheManager: cache,
          fit: BoxFit.contain,
          errorWidget: (_, _, _) => const SizedBox(),
        ),
        const Center(child: CircularProgressIndicator()),
      ]),
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

  /// The computer answers but this phone can't play the video (its format).
  bool _unplayable = false;
  Timer? _retry;

  @override
  void initState() {
    super.initState();
    if (widget.active) _start();
  }

  @override
  void didUpdateWidget(VideoPage old) {
    super.didUpdateWidget(old);
    if (widget.active && _video == null && _retry == null && !_unplayable) _start();
    if (!widget.active) _video?.pause();
  }

  Future<void> _start() async {
    _retry = null;
    VideoPlayerController? v;
    String? url;
    try {
      url = await widget.api.mediaUrl(widget.item.id);
      v = VideoPlayerController.networkUrl(Uri.parse(url));
      await v.initialize();
      if (!mounted) return v.dispose();
      _video = v;
      v.addListener(() => mounted ? setState(() {}) : null);
      if (widget.active) await v.play();
      setState(() {});
    } catch (_) {
      await v?.dispose();
      if (!mounted) return;
      // The player failed although the computer serves the file: the phone can't decode it.
      // Anything else (computer off, or just started and still scanning): wait and try again.
      if (url != null && await _serves(url)) {
        if (mounted) setState(() => _unplayable = true);
      } else if (mounted) {
        _retry = Timer(retryAfter, _start);
        setState(() {});
      }
    }
  }

  /// Whether the media link answers with the video's first bytes.
  static Future<bool> _serves(String url) async {
    final client = HttpClient()..connectionTimeout = const Duration(seconds: 4);
    try {
      final req = await client.getUrl(Uri.parse(url));
      req.headers.set(HttpHeaders.rangeHeader, 'bytes=0-1');
      final resp = await req.close().timeout(const Duration(seconds: 6));
      await resp.drain<void>();
      return resp.statusCode == 206 || resp.statusCode == 200;
    } catch (_) {
      return false;
    } finally {
      client.close(force: true);
    }
  }

  @override
  void dispose() {
    _retry?.cancel();
    _video?.dispose();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final v = _video;
    if (_unplayable) {
      return Stack(alignment: Alignment.center, children: [
        Positioned.fill(child: Opacity(opacity: 0.35, child: ThumbWithSpinner(item: widget.item, api: widget.api, cache: widget.cache))),
        Container(
          margin: const EdgeInsets.all(32),
          padding: const EdgeInsets.all(16),
          decoration: BoxDecoration(color: Colors.black87, borderRadius: BorderRadius.circular(14)),
          child: const Text("This phone can't play this video's format.\nOpen it on the computer instead.", textAlign: TextAlign.center),
        ),
      ]);
    }
    if (v == null || !v.value.isInitialized) {
      return ThumbWithSpinner(item: widget.item, api: widget.api, cache: widget.cache);
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
