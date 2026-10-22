// Pairing with the computer: scan the QR code under Connect phone (or open its link).

import 'dart:io';

import 'package:device_info_plus/device_info_plus.dart';
import 'package:flutter/material.dart';
import 'package:mobile_scanner/mobile_scanner.dart';

import '../api.dart';
import '../main.dart' show banana;

class ConnectScreen extends StatefulWidget {
  const ConnectScreen({super.key, this.link, this.notice, required this.onPaired, this.onCancel, this.onFailed});

  /// A pairing link to use right away (opened from the camera app).
  final String? link;

  /// Why the phone is here again (the computer removed it).
  final String? notice;
  final void Function(Pairing) onPaired;
  final VoidCallback? onCancel;

  /// Pairing with [link] failed, so the same link may be tried again.
  final VoidCallback? onFailed;

  @override
  State<ConnectScreen> createState() => _ConnectScreenState();
}

class _ConnectScreenState extends State<ConnectScreen> {
  bool _busy = false;
  String? _error;
  final _paste = TextEditingController();

  @override
  void initState() {
    super.initState();
    if (widget.link != null) WidgetsBinding.instance.addPostFrameCallback((_) => _pair(widget.link!));
  }

  /// A new link while this screen is up (the first code had expired and a new one was scanned).
  @override
  void didUpdateWidget(ConnectScreen old) {
    super.didUpdateWidget(old);
    final link = widget.link;
    if (link != null && link != old.link && !_busy) _pair(link);
  }

  Future<String> _phoneName() async {
    final info = DeviceInfoPlugin();
    try {
      if (Platform.isAndroid) {
        final a = await info.androidInfo;
        final brand = a.brand.isEmpty ? '' : '${a.brand[0].toUpperCase()}${a.brand.substring(1)} ';
        return '$brand${a.model}'.trim();
      }
      if (Platform.isIOS) return (await info.iosInfo).name;
    } catch (_) {}
    return 'Phone';
  }

  Future<void> _pair(String link) async {
    final p = Pairing.fromLink(link);
    if (p == null) {
      setState(() => _error = "That isn't a Banana Photos code. On the computer, open Connect phone and scan the code shown there.");
      return;
    }
    setState(() {
      _busy = true;
      _error = null;
    });
    try {
      final paired = await Api.pair(p, await _phoneName());
      widget.onPaired(paired);
    } catch (e) {
      setState(() => _error = '$e');
      widget.onFailed?.call();
    } finally {
      if (mounted) setState(() => _busy = false);
    }
  }

  Future<void> _scan() async {
    final link = await Navigator.of(context).push<String>(MaterialPageRoute(builder: (_) => const ScannerScreen()));
    if (link != null) await _pair(link);
  }

  @override
  Widget build(BuildContext context) {
    final t = Theme.of(context).textTheme;
    return Scaffold(
      appBar: widget.onCancel == null ? null : AppBar(leading: CloseButton(onPressed: widget.onCancel)),
      body: SafeArea(
        child: ListView(
          padding: const EdgeInsets.all(28),
          children: [
            const SizedBox(height: 32),
            const Icon(Icons.photo_library_outlined, size: 64, color: banana),
            const SizedBox(height: 20),
            Text('Banana Photos', style: t.headlineMedium, textAlign: TextAlign.center),
            const SizedBox(height: 12),
            Text(
              'See the photos on your computer, and back up this phone to it, over your own Wi-Fi.',
              style: t.bodyLarge?.copyWith(color: Colors.white70),
              textAlign: TextAlign.center,
            ),
            if (widget.notice != null) ...[
              const SizedBox(height: 20),
              Card(
                child: Padding(
                  padding: const EdgeInsets.all(14),
                  child: Row(children: [
                    const Icon(Icons.link_off),
                    const SizedBox(width: 12),
                    Expanded(child: Text(widget.notice!)),
                  ]),
                ),
              ),
            ],
            const SizedBox(height: 32),
            for (final step in const [
              '1. On the computer, open Banana Photos → Connect phone and turn it on.',
              '2. Put this phone on the same Wi-Fi.',
              '3. Scan the code on the computer\'s screen.',
            ])
              Padding(padding: const EdgeInsets.only(bottom: 8), child: Text(step, style: t.bodyMedium?.copyWith(color: Colors.white70))),
            const SizedBox(height: 24),
            if (_busy)
              const Center(child: Padding(padding: EdgeInsets.all(12), child: CircularProgressIndicator()))
            else
              FilledButton.icon(onPressed: _scan, icon: const Icon(Icons.qr_code_scanner), label: const Text('Scan code')),
            if (_error != null) ...[
              const SizedBox(height: 16),
              Text(_error!, style: t.bodyMedium?.copyWith(color: Theme.of(context).colorScheme.error)),
            ],
            const SizedBox(height: 32),
            ExpansionTile(
              title: const Text('No camera? Paste the link'),
              childrenPadding: const EdgeInsets.symmetric(vertical: 8),
              children: [
                TextField(controller: _paste, decoration: const InputDecoration(hintText: 'bananaphotos://pair?…', border: OutlineInputBorder())),
                const SizedBox(height: 8),
                Align(alignment: Alignment.centerRight, child: TextButton(onPressed: _busy ? null : () => _pair(_paste.text), child: const Text('Connect'))),
              ],
            ),
          ],
        ),
      ),
    );
  }
}

/// The camera, until it sees a Banana Photos pairing code.
class ScannerScreen extends StatefulWidget {
  const ScannerScreen({super.key});

  @override
  State<ScannerScreen> createState() => _ScannerScreenState();
}

class _ScannerScreenState extends State<ScannerScreen> {
  bool _done = false;

  @override
  Widget build(BuildContext context) {
    return Scaffold(
      appBar: AppBar(title: const Text('Scan the code on the computer')),
      body: MobileScanner(
        onDetect: (capture) {
          for (final b in capture.barcodes) {
            final v = b.rawValue;
            if (!_done && v != null && v.startsWith('bananaphotos://')) {
              _done = true;
              Navigator.of(context).pop(v);
              return;
            }
          }
        },
      ),
    );
  }
}
