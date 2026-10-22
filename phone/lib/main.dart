// Banana Photos on the phone: the library on your computer, over your own Wi-Fi.

import 'dart:async';

import 'package:app_links/app_links.dart';
import 'package:flutter/material.dart';

import 'api.dart';
import 'screens/connect.dart';
import 'screens/home.dart';

void main() => runApp(const BananaApp());

const banana = Color(0xFFFDD663);

class BananaApp extends StatefulWidget {
  const BananaApp({super.key});

  @override
  State<BananaApp> createState() => _BananaAppState();
}

class _BananaAppState extends State<BananaApp> {
  Pairing? _pairing;
  bool _loaded = false;

  /// A pairing link the phone's camera app opened us with, waiting to be used.
  String? _link;
  StreamSubscription<Uri>? _links;

  @override
  void initState() {
    super.initState();
    Pairing.load().then((p) => setState(() {
          _pairing = p?.token == null ? null : p;
          _loaded = true;
        }));
    final links = AppLinks();
    links.getInitialLink().then((u) => _openLink(u));
    _links = links.uriLinkStream.listen(_openLink);
  }

  void _openLink(Uri? uri) {
    if (uri != null && uri.scheme == 'bananaphotos') setState(() => _link = uri.toString());
  }

  /// After a failed pairing the same link may come again (scanned twice): let it through.
  void _clearLink() => _link = null;

  @override
  void dispose() {
    _links?.cancel();
    super.dispose();
  }

  @override
  Widget build(BuildContext context) {
    final theme = ThemeData(
      colorScheme: ColorScheme.fromSeed(seedColor: banana, brightness: Brightness.dark, surface: const Color(0xFF131314)),
      useMaterial3: true,
    );
    Widget home;
    if (!_loaded) {
      home = const Scaffold(body: Center(child: CircularProgressIndicator()));
    } else if (_pairing == null || _link != null) {
      home = ConnectScreen(
        link: _link,
        onPaired: (p) => setState(() {
          _pairing = p;
          _link = null;
        }),
        onCancel: _pairing == null ? null : () => setState(() => _link = null),
        onFailed: _clearLink,
      );
    } else {
      home = HomeScreen(
        key: ValueKey(_pairing!.fingerprint + (_pairing!.token ?? '')),
        pairing: _pairing!,
        onUnpaired: () async {
          await Pairing.forget();
          setState(() => _pairing = null);
        },
      );
    }
    return MaterialApp(title: 'Banana Photos', theme: theme, darkTheme: theme, home: home, debugShowCheckedModeBanner: false);
  }
}
