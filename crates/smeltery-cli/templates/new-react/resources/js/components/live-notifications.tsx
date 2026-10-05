import { router, usePage } from '@inertiajs/react';
import { echo, useEcho } from '@laravel/echo-react';
import { useEffect, useState } from 'react';

import Card from '@/components/card';

/** `UserNotified` in `app/events/user_notified.rs`: the event's data. */
interface UserNotified {
    message: string;
}

/** A member of `presence-dashboard`: `routes/channels.rs` shares only the id (add `name` there to list names). */
interface Member {
    id: number;
}

/**
 * The signed-in user's private channel `users.<id>` (only they may join it, `routes/channels.rs`) and the presence
 * channel `dashboard` (how many people have the dashboard open). "Notify me" asks the server to send this user an
 * event.
 */
export default function LiveNotifications() {
    const user = usePage().props.auth.user;
    const id = user?.id ?? 0;
    const [messages, setMessages] = useState<string[]>([]);
    const [online, setOnline] = useState<Member[]>([]);
    const [sending, setSending] = useState(false);

    useEcho<UserNotified>(`users.${id}`, 'UserNotified', (event) => setMessages((list) => [event.message, ...list].slice(0, 5)), [id]);

    useEffect(() => {
        echo()
            .join('dashboard')
            .here((members: Member[]) => setOnline(members))
            .joining((member: Member) => setOnline((list) => [...list.filter((m) => m.id !== member.id), member]))
            .leaving((member: Member) => setOnline((list) => list.filter((m) => m.id !== member.id)));
        return () => echo().leave('dashboard');
    }, []);

    function notifyMe() {
        router.post('/notify-me', {}, { preserveScroll: true, onStart: () => setSending(true), onFinish: () => setSending(false) });
    }

    return (
        <Card title="Live">
            <p className="font-medium">
                {online.length > 0 ? `${online.length} ${online.length === 1 ? 'person' : 'people'} online · you: ${user?.name ?? ''}` : 'connecting…'}
            </p>
            <button type="button" className="btn-secondary mt-3" onClick={notifyMe} disabled={sending}>
                Notify me
            </button>
            <ul className="mt-3 space-y-1" aria-live="polite">
                {messages.map((message, i) => (
                    <li key={i}>{message}</li>
                ))}
            </ul>
        </Card>
    );
}
