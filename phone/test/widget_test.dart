import 'package:banana_photos/api.dart';
import 'package:banana_photos/library.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  final fp = 'ab' * 32;

  test('reads the pairing link from the QR code', () {
    final p = Pairing.fromLink('bananaphotos://pair?v=1&n=Aqib%27s%20PC&h=192.168.1.5%2C10.0.0.2&p=47820&m=47821&c=0123456789abcdef&f=$fp')!;
    expect(p.name, "Aqib's PC");
    expect(p.hosts, ['192.168.1.5', '10.0.0.2']);
    expect((p.port, p.mediaPort, p.code, p.fingerprint), (47820, 47821, '0123456789abcdef', fp));
  });

  test('rejects links that are not pairing codes', () {
    expect(Pairing.fromLink('https://example.com'), isNull);
    expect(Pairing.fromLink('bananaphotos://pair?v=2&h=1.2.3.4&p=1&c=x&f=$fp'), isNull);
    expect(Pairing.fromLink('bananaphotos://pair?v=1&h=1.2.3.4&p=1&c=x&f=short'), isNull);
    expect(Pairing.fromLink('bananaphotos://pair?v=1&h=&p=1&c=x&f=$fp'), isNull);
  });

  test('dates round-trip as wall-clock time', () {
    final local = DateTime(2024, 10, 6, 23, 30, 5);
    final back = fromWallClockSeconds(wallClockSeconds(local));
    expect((back.year, back.month, back.day, back.hour, back.minute, back.second), (2024, 10, 6, 23, 30, 5));
  });

  test('lays out days with four photos to a row', () {
    Item item(String id, int day) => Item.fromJson(
        {'id': id, 'name': '$id.jpg', 'taken': wallClockSeconds(DateTime(2024, 5, day, 12)), 'aspect': 1.0, 'size': 1, 'video': false, 'favorite': false});
    final items = [for (var i = 0; i < 6; i++) item('a$i', 2), item('b', 1)];
    final rows = layoutRows(items, 4);
    expect(rows.length, 5);
    expect(rows[0], isA<DayRow>().having((r) => r.count, 'count', 6));
    expect(rows[1], isA<PhotosRow>().having((r) => (r.start, r.end), 'range', (0, 4)));
    expect(rows[2], isA<PhotosRow>().having((r) => (r.start, r.end), 'range', (4, 6)));
    expect(rows[3], isA<DayRow>().having((r) => r.count, 'count', 1));
    expect(rows[4], isA<PhotosRow>().having((r) => (r.start, r.end), 'range', (6, 7)));
  });
}
