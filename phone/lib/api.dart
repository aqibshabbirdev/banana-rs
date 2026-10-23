// Talking to Banana Photos on the computer: pairing, finding it on the network, and the API
// in docs/phone-api.md. Every connection trusts exactly one certificate, the one whose SHA-256
// the QR code carried.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:bonsoir/bonsoir.dart';
import 'package:crypto/crypto.dart';
import 'package:flutter_secure_storage/flutter_secure_storage.dart';
import 'package:http/http.dart' as http;
import 'package:http/io_client.dart';

const serviceType = '_bananaphotos._tcp';

/// What the phone keeps about the computer it's paired with.
class Pairing {
  Pairing({required this.name, required this.hosts, required this.port, required this.mediaPort, required this.fingerprint, this.code, this.token});

  final String name;

  /// Addresses to try, the last one that worked first.
  List<String> hosts;
  int port;
  int mediaPort;
  final String fingerprint;

  /// The one-time code from the QR link; only until it's traded for [token].
  final String? code;
  String? token;

  /// Reads a `bananaphotos://pair?...` link (from the QR code or a tapped link).
  static Pairing? fromLink(String link) {
    final uri = Uri.tryParse(link.trim());
    if (uri == null || uri.scheme != 'bananaphotos' || uri.host != 'pair') return null;
    final q = uri.queryParameters;
    final hosts = (q['h'] ?? '').split(',').where((h) => h.isNotEmpty).toList();
    final port = int.tryParse(q['p'] ?? '');
    final fp = (q['f'] ?? '').toLowerCase();
    if (q['v'] != '1' || hosts.isEmpty || port == null || fp.length != 64 || (q['c'] ?? '').isEmpty) return null;
    return Pairing(
      name: q['n'] ?? 'Computer',
      hosts: hosts,
      port: port,
      mediaPort: int.tryParse(q['m'] ?? '') ?? port + 1,
      fingerprint: fp,
      code: q['c'],
    );
  }

  Map<String, dynamic> toJson() => {'name': name, 'hosts': hosts, 'port': port, 'mediaPort': mediaPort, 'fingerprint': fingerprint, 'token': token};

  static Pairing fromJson(Map<String, dynamic> j) => Pairing(
        name: j['name'] as String,
        hosts: (j['hosts'] as List).cast<String>(),
        port: j['port'] as int,
        mediaPort: j['mediaPort'] as int,
        fingerprint: j['fingerprint'] as String,
        token: j['token'] as String?,
      );

  static const _storage = FlutterSecureStorage();

  static Future<Pairing?> load() async {
    final raw = await _storage.read(key: 'pairing');
    if (raw == null) return null;
    try {
      return Pairing.fromJson(jsonDecode(raw) as Map<String, dynamic>);
    } catch (_) {
      return null;
    }
  }

  Future<void> save() => _storage.write(key: 'pairing', value: jsonEncode(toJson()));

  static Future<void> forget() => _storage.delete(key: 'pairing');
}

class ApiException implements Exception {
  ApiException(this.status, this.message);
  final int status;
  final String message;

  /// The computer no longer knows this phone (removed there): pair again.
  bool get unpaired => status == 401;

  @override
  String toString() => message;
}

/// An HTTP client that accepts only the certificate with this SHA-256 (no CA is trusted).
HttpClient pinnedHttpClient(String fingerprint) {
  final client = HttpClient(context: SecurityContext(withTrustedRoots: false))
    ..connectionTimeout = const Duration(seconds: 5)
    ..idleTimeout = const Duration(seconds: 20);
  client.badCertificateCallback = (cert, host, port) => sha256.convert(cert.der).toString() == fingerprint;
  return client;
}

/// Retries a GET once when the connection it reused turns out closed (the computer closes idle
/// connections, and restarts).
class _RetryClient extends http.BaseClient {
  _RetryClient(this._inner);
  final http.Client _inner;

  @override
  Future<http.StreamedResponse> send(http.BaseRequest request) async {
    try {
      return await _inner.send(request);
    } on Exception catch (e) {
      if (request.method != 'GET' || !(e is SocketException || e is http.ClientException || e is HttpException)) rethrow;
      final again = http.Request('GET', request.url)..headers.addAll(request.headers);
      return _inner.send(again);
    }
  }

  @override
  void close() => _inner.close();
}

class Api {
  Api(this.pairing)
      : _io = pinnedHttpClient(pairing.fingerprint),
        client = _RetryClient(IOClient(pinnedHttpClient(pairing.fingerprint)));

  final Pairing pairing;
  final HttpClient _io;

  /// For packages that take a `package:http` client (the image cache).
  final http.Client client;

  String get host => pairing.hosts.first;

  Map<String, String> get headers => {'Authorization': 'Bearer ${pairing.token}'};

  Uri uri(String path, [Map<String, String>? query]) {
    // IPv6 literals need brackets in URLs.
    final h = host.contains(':') ? '[$host]' : host;
    return Uri.parse('https://$h:${pairing.port}$path').replace(queryParameters: query);
  }

  String thumbUrl(String id) => uri('/api/thumb/$id').toString();
  String photoUrl(String id, {int max = 2048}) => uri('/api/photo/$id', {'max': '$max'}).toString();

  /// Trades the QR code's one-time code for a token, trying each address in the link.
  static Future<Pairing> pair(Pairing p, String phoneName) async {
    Object? lastError;
    for (final h in List.of(p.hosts)) {
      final api = Api(p..hosts = [h, ...p.hosts.where((x) => x != h)]);
      try {
        final j = await api._send('POST', '/api/pair', body: jsonEncode({'code': p.code, 'name': phoneName}), auth: false);
        p.token = j['token'] as String;
        await p.save();
        return p;
      } on ApiException {
        rethrow; // The computer answered: a wrong or used code won't work on another address.
      } catch (e) {
        lastError = e;
      }
    }
    throw ApiException(0, "Couldn't reach ${p.name}. Is the phone on the same Wi-Fi? ($lastError)");
  }

  /// Makes sure the computer answers, at the saved address or, failing that, by looking for
  /// it on the network (mDNS) by its certificate fingerprint. Returns /api/info.
  Future<Map<String, dynamic>> connect() async {
    for (final h in List.of(pairing.hosts)) {
      pairing.hosts = [h, ...pairing.hosts.where((x) => x != h)];
      try {
        return await info();
      } on ApiException {
        rethrow;
      } catch (_) {}
    }
    final found = await discover(pairing.fingerprint);
    if (found != null) {
      pairing.hosts = [found.$1, ...pairing.hosts.where((x) => x != found.$1)];
      pairing.port = found.$2;
      pairing.mediaPort = found.$3;
      final j = await info();
      await pairing.save();
      return j;
    }
    throw ApiException(0, "Can't find ${pairing.name} on this network. Is Banana Photos open there, with Connect phone on?");
  }

  Future<Map<String, dynamic>> info() => getJson('/api/info');

  Future<Map<String, dynamic>> getJson(String path, [Map<String, String>? query]) => _send('GET', path, query: query);

  Future<Map<String, dynamic>> _send(String method, String path, {Map<String, String>? query, String? body, bool auth = true}) async {
    try {
      return await _sendOnce(method, path, query: query, body: body, auth: auth);
    } on SocketException {
      if (method != 'GET') rethrow;
      return _sendOnce(method, path, query: query, body: body, auth: auth);
    } on HttpException {
      if (method != 'GET') rethrow;
      return _sendOnce(method, path, query: query, body: body, auth: auth);
    }
  }

  Future<Map<String, dynamic>> _sendOnce(String method, String path, {Map<String, String>? query, String? body, bool auth = true}) async {
    final req = await _io.openUrl(method, uri(path, query)).timeout(const Duration(seconds: 8));
    if (auth) req.headers.set('Authorization', 'Bearer ${pairing.token}');
    if (body != null) {
      req.headers.contentType = ContentType.json;
      final bytes = utf8.encode(body);
      req.contentLength = bytes.length;
      req.add(bytes);
    }
    final resp = await req.close().timeout(const Duration(seconds: 60));
    final text = await resp.transform(utf8.decoder).join();
    final j = text.isEmpty ? <String, dynamic>{} : jsonDecode(text) as Map<String, dynamic>;
    if (resp.statusCode != 200) throw ApiException(resp.statusCode, (j['error'] as String?) ?? 'Error ${resp.statusCode}');
    return j;
  }

  /// The whole timeline, newest first, in pages.
  Future<({int version, List<Map<String, dynamic>> items})> photos() async {
    final items = <Map<String, dynamic>>[];
    var version = 0;
    while (true) {
      final page = await getJson('/api/photos', {'offset': '${items.length}', 'limit': '5000'});
      version = page['library_version'] as int;
      final got = (page['items'] as List).cast<Map<String, dynamic>>();
      items.addAll(got);
      if (got.isEmpty || items.length >= (page['total'] as int)) break;
    }
    return (version: version, items: items);
  }

  Future<({String kind, List<String> ids})> search(String q) async {
    final j = await getJson('/api/search', {'q': q});
    return (kind: j['kind'] as String, ids: (j['ids'] as List).cast<String>());
  }

  /// Albums and people: `id`, `name` (null for an unnamed person), `count`, `cover` (a photo id).
  Future<List<Map<String, dynamic>>> albums() async => ((await getJson('/api/albums'))['albums'] as List).cast<Map<String, dynamic>>();
  Future<List<Map<String, dynamic>>> people() async => ((await getJson('/api/people'))['people'] as List).cast<Map<String, dynamic>>();

  /// The photos of an album or person (`album` or `person`), newest first.
  Future<List<String>> members(String kind, String id) async => ((await getJson('/api/$kind/$id'))['ids'] as List).cast<String>();

  String faceUrl(String personId) => uri('/api/face/$personId').toString();

  /// A plain-HTTP link to the video for the player, signed for this one file.
  Future<String> mediaUrl(String id) async => (await getJson('/api/media-url/$id'))['url'] as String;

  /// The video converted on the computer for phones that can't play it: `status` is `ready`
  /// (with `url`), `converting` (with `progress`, 0–1) or `failed` (with `error`).
  Future<Map<String, dynamic>> compat(String id) => getJson('/api/compat/$id');

  Future<void> favorite(String id, bool on) => _send('POST', '/api/favorite/$id', query: {'on': on ? '1' : '0'});

  /// Favourites (or not) several at once.
  Future<void> favorites(List<String> ids, bool on) => _send('POST', '/api/favorites', body: jsonEncode({'ids': ids, 'on': on}));

  /// Moves photos to the computer's Trash. Returns how many went, and why others didn't.
  Future<({int trashed, int skipped, List<String> errors})> trash(List<String> ids) async {
    final j = await _send('POST', '/api/trash', body: jsonEncode({'ids': ids}));
    return (trashed: j['trashed'] as int, skipped: j['skipped'] as int, errors: (j['errors'] as List).cast<String>());
  }

  Future<bool> has(String name, int size, int taken) async =>
      (await getJson('/api/has', {'name': name, 'size': '$size', 'taken': '$taken'}))['have'] == true;

  /// Sends one file from the phone; returns "imported" or "skipped".
  Future<String> upload(File file, String name, int taken, {void Function(int sent, int total)? progress}) async {
    final length = await file.length();
    final req = await _io.openUrl('POST', uri('/api/upload', {'name': name, 'taken': '$taken'}));
    req.headers.set('Authorization', 'Bearer ${pairing.token}');
    req.headers.contentType = ContentType.binary;
    req.contentLength = length;
    var sent = 0;
    await req.addStream(file.openRead().map((chunk) {
      sent += chunk.length;
      progress?.call(sent, length);
      return chunk;
    }));
    final resp = await req.close();
    final text = await resp.transform(utf8.decoder).join();
    final j = text.isEmpty ? <String, dynamic>{} : jsonDecode(text) as Map<String, dynamic>;
    if (resp.statusCode != 200) throw ApiException(resp.statusCode, (j['error'] as String?) ?? 'Error ${resp.statusCode}');
    return j['status'] as String;
  }

  /// Downloads a photo or video to [to]: the file as stored ([original]), or the photo as a
  /// large JPEG.
  Future<File> download(String id, File to, {bool original = true, void Function(int got, int total)? progress}) async {
    final req = await _io.getUrl(uri('/api/photo/$id', original ? {'original': '1'} : {'max': '4096'}));
    req.headers.set('Authorization', 'Bearer ${pairing.token}');
    final resp = await req.close();
    if (resp.statusCode != 200) {
      final text = await resp.transform(utf8.decoder).join();
      String? why;
      try {
        why = (jsonDecode(text) as Map<String, dynamic>)['error'] as String?;
      } catch (_) {}
      throw ApiException(resp.statusCode, why ?? 'Error ${resp.statusCode}');
    }
    var got = 0;
    await resp.map((chunk) {
      got += chunk.length;
      progress?.call(got, resp.contentLength);
      return chunk;
    }).pipe(to.openWrite());
    return to;
  }

  void close() {
    client.close();
    _io.close(force: true);
  }
}

/// Looks for the computer with this certificate fingerprint on the local network for a few
/// seconds. Returns (address, port, media port).
Future<(String, int, int)?> discover(String fingerprint, {Duration timeout = const Duration(seconds: 8)}) async {
  final discovery = BonsoirDiscovery(type: serviceType);
  final done = Completer<(String, int, int)?>();
  StreamSubscription? sub;
  try {
    await discovery.initialize();
    sub = discovery.eventStream?.listen((event) {
      switch (event) {
        case BonsoirDiscoveryServiceFoundEvent():
          event.service.resolve(discovery.serviceResolver);
        case BonsoirDiscoveryServiceResolvedEvent():
          final s = event.service;
          final addresses = s.hostAddresses.where((a) => !a.contains(':')).toList() + s.hostAddresses.where((a) => a.contains(':')).toList();
          if (s.attributes['f'] == fingerprint && addresses.isNotEmpty && !done.isCompleted) {
            done.complete((addresses.first, s.port, int.tryParse(s.attributes['m'] ?? '') ?? s.port + 1));
          }
        default:
      }
    });
    await discovery.start();
    return await done.future.timeout(timeout, onTimeout: () => null);
  } catch (_) {
    return null;
  } finally {
    await sub?.cancel();
    await discovery.stop();
  }
}

/// Seconds since 1970 of a local wall-clock time, written as if it were UTC: the API's dates.
int wallClockSeconds(DateTime local) =>
    DateTime.utc(local.year, local.month, local.day, local.hour, local.minute, local.second).millisecondsSinceEpoch ~/ 1000;

/// The API's dates back as a wall-clock time (a UTC DateTime whose fields are the local time).
DateTime fromWallClockSeconds(int s) => DateTime.fromMillisecondsSinceEpoch(s * 1000, isUtc: true);
