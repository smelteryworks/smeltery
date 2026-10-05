// Offline checks: no server needed.
import 'package:anvil_link/anvil_link.dart';
import 'package:test/test.dart';

void main() {
  test('the socket URL is ws://host:port/app/<key> with protocol 7', () async {
    final s = AnvilSession(baseUrl: Uri.parse('http://127.0.0.1:8471'), appKey: 'abc', token: 't');
    final uri = s.options.uri;
    expect(uri.scheme, 'ws');
    expect(uri.host, '127.0.0.1');
    expect(uri.port, 8471);
    expect(uri.path, '/app/abc');
    expect(uri.queryParameters['protocol'], '7');
    expect(s.authEndpoint.toString(), 'http://127.0.0.1:8471/api/broadcasting/auth');
    await s.close();
  });

  test('https becomes wss on the default port', () async {
    final s = AnvilSession(baseUrl: Uri.parse('https://app.example.test'), appKey: 'abc', token: 't');
    expect(s.options.uri.scheme, 'wss');
    expect(s.options.uri.hasPort, isFalse);
    await s.close();
  });

  test('a client event needs a private or presence channel', () async {
    final s = AnvilSession(baseUrl: Uri.parse('http://127.0.0.1:1'), appKey: 'abc', token: 't');
    expect(() => s.whisper(s.public('news'), 'typing', {}), throwsArgumentError);
    await s.close();
  });
}
