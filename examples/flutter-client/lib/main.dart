import 'dart:io';

import 'package:flutter/material.dart';

import 'anvil_controller.dart';

/// The Smeltery app to connect to: `--dart-define=SMELTERY_URL=...`. The Android emulator reaches the host's
/// 127.0.0.1 at 10.0.2.2.
String defaultUrl() {
  const fromEnv = String.fromEnvironment('SMELTERY_URL');
  if (fromEnv.isNotEmpty) return fromEnv;
  return Platform.isAndroid ? 'http://10.0.2.2:8000' : 'http://127.0.0.1:8000';
}

const appKey = String.fromEnvironment('ANVIL_APP_KEY', defaultValue: 'smeltery-example-key');
const email = String.fromEnvironment('SMELTERY_EMAIL');
const password = String.fromEnvironment('SMELTERY_PASSWORD');

void main() => runApp(const AnvilApp());

class AnvilApp extends StatelessWidget {
  const AnvilApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'Anvil client',
        theme: ThemeData(colorSchemeSeed: Colors.deepOrange, useMaterial3: true),
        home: AnvilPage(controller: AnvilController(baseUrl: Uri.parse(defaultUrl()), appKey: appKey)),
      );
}

class AnvilPage extends StatefulWidget {
  const AnvilPage({super.key, required this.controller});

  final AnvilController controller;

  @override
  State<AnvilPage> createState() => _AnvilPageState();
}

class _AnvilPageState extends State<AnvilPage> {
  final _email = TextEditingController(text: email);
  final _password = TextEditingController(text: password);
  final _room = TextEditingController(text: '1');
  final _message = TextEditingController();

  AnvilController get c => widget.controller;

  @override
  void dispose() {
    c.dispose();
    for (final t in [_email, _password, _room, _message]) {
      t.dispose();
    }
    super.dispose();
  }

  @override
  Widget build(BuildContext context) => Scaffold(
        appBar: AppBar(title: Text('Anvil: ${c.baseUrl}')),
        body: ListenableBuilder(
          listenable: c,
          builder: (context, _) => Padding(
            padding: const EdgeInsets.all(12),
            child: Column(
              crossAxisAlignment: CrossAxisAlignment.stretch,
              children: [
                if (!c.connected) ...[
                  TextField(key: const Key('email'), controller: _email, decoration: const InputDecoration(labelText: 'E-mail')),
                  TextField(
                    key: const Key('password'),
                    controller: _password,
                    obscureText: true,
                    decoration: const InputDecoration(labelText: 'Password'),
                  ),
                  TextField(key: const Key('room'), controller: _room, decoration: const InputDecoration(labelText: 'Room')),
                  const SizedBox(height: 8),
                  FilledButton(
                    key: const Key('connect'),
                    onPressed: () => c.connect(
                      email: _email.text.trim(),
                      password: _password.text,
                      room: int.tryParse(_room.text) ?? 1,
                    ),
                    child: const Text('Connect'),
                  ),
                ] else
                  Wrap(spacing: 8, children: [
                    OutlinedButton(onPressed: () => c.announce('hello from Flutter'), child: const Text('Announce')),
                    OutlinedButton(onPressed: c.signOut, child: const Text('Sign out')),
                    
                  ]),
                const SizedBox(height: 8),
                Text(c.status, key: const Key('status')),
                const SizedBox(height: 8),
                Text('In the room:', style: Theme.of(context).textTheme.titleSmall),
                Wrap(spacing: 6, children: [for (final m in c.members.entries) Chip(label: Text('${m.key}: ${m.value}'))]),
                Row(children: [
                  Expanded(
                    child: TextField(
                      key: const Key('whisper'),
                      controller: _message,
                      decoration: const InputDecoration(labelText: 'Whisper to the room'),
                    ),
                  ),
                  IconButton(
                    icon: const Icon(Icons.send),
                    onPressed: () {
                      c.whisper(_message.text);
                      _message.clear();
                    },
                  ),
                ]),
                const Divider(),
                Expanded(
                  child: ListView(
                    key: const Key('events'),
                    children: [for (final e in c.events) Text(e, style: const TextStyle(fontFamily: 'monospace', fontSize: 12))],
                  ),
                ),
              ],
            ),
          ),
        ),
      );
}
