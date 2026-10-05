import 'package:flutter/material.dart';
import 'package:flutter_client/anvil_controller.dart';
import 'package:flutter_client/main.dart';
import 'package:flutter_test/flutter_test.dart';

void main() {
  testWidgets('the sign-in form and the empty room render before connecting', (tester) async {
    final controller = AnvilController(baseUrl: Uri.parse('http://127.0.0.1:1'), appKey: 'k');
    await tester.pumpWidget(MaterialApp(home: AnvilPage(controller: controller)));
    expect(find.byKey(const Key('email')), findsOneWidget);
    expect(find.byKey(const Key('connect')), findsOneWidget);
    expect(find.text('signed out'), findsOneWidget);
    expect(find.byType(Chip), findsNothing);
  });
}
