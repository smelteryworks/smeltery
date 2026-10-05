// On a device or emulator against a running Smeltery app (Android emulator: the host's 127.0.0.1 is 10.0.2.2):
//   flutter test integration_test -d <device> --dart-define=SMELTERY_URL=http://10.0.2.2:8000
import 'package:flutter/material.dart';
import 'package:flutter_client/main.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';

const testEmail = String.fromEnvironment('SMELTERY_EMAIL', defaultValue: 'demo@example.com');
const testPassword = String.fromEnvironment('SMELTERY_PASSWORD', defaultValue: 'password');

Future<void> pumpUntil(WidgetTester tester, bool Function() done, {int seconds = 20}) async {
  for (var i = 0; i < seconds * 10 && !done(); i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 100)));
    await tester.pump();
  }
}

String statusText(WidgetTester tester) => tester.widget<Text>(find.byKey(const Key('status'))).data ?? '';

bool hasEvent(String part) => find.textContaining(part).evaluate().isNotEmpty;

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  testWidgets('signs in, subscribes the three channels, receives an announcement and signs out', (tester) async {
    await tester.pumpWidget(const AnvilApp());
    await tester.enterText(find.byKey(const Key('email')), testEmail);
    await tester.enterText(find.byKey(const Key('password')), testPassword);
    await tester.tap(find.byKey(const Key('connect')));
    await pumpUntil(tester, () => hasEvent(': ok') && find.textContaining('presence-chat.1: ok').evaluate().isNotEmpty);
    expect(statusText(tester), startsWith('connected as user'));
    expect(find.textContaining('announcements: ok'), findsOneWidget);
    expect(find.textContaining(RegExp(r'private-users\.\d+: ok')), findsOneWidget);
    expect(find.textContaining('presence-chat.1: ok'), findsOneWidget);

    await tester.tap(find.text('Announce'));
    await pumpUntil(tester, () => hasEvent('hello from Flutter'));
    expect(find.textContaining(r'App\Events\AnnouncementPosted'), findsWidgets);

    // The room's member list shows the signed-in user's name (from the subscription's member list).
    expect(find.textContaining(': Demo User'), findsOneWidget);

    await tester.tap(find.text('Sign out'));
    await pumpUntil(tester, () => statusText(tester) == 'signed out');
    expect(find.textContaining('DELETE /api/tokens/current: 204'), findsOneWidget);
    expect(find.byKey(const Key('connect')), findsOneWidget);
  });
}
