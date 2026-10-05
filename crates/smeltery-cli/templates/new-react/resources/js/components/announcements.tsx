import { useEchoPublic } from '@laravel/echo-react';
import { useState } from 'react';

import Card from '@/components/card';

/** `AnnouncementPosted` in `app/events/announcement_posted.rs`: the event's data. */
interface AnnouncementPosted {
    message: string;
}

/** The public `announcements` channel, live: the newest five announcements, as the server broadcasts them. */
export default function Announcements() {
    const [messages, setMessages] = useState<string[]>([]);
    useEchoPublic<AnnouncementPosted>('announcements', 'AnnouncementPosted', (event) => {
        setMessages((list) => [event.message, ...list].slice(0, 5));
    });
    return (
        <Card title="Announcements">
            {messages.length === 0 ? (
                <p>
                    Live from the <code>announcements</code> channel: a handler, a job or an agent sends one with{' '}
                    <code>{'anvil.send(&AnnouncementPosted { message })'}</code>.
                </p>
            ) : (
                <ul className="space-y-1" aria-live="polite">
                    {messages.map((message, i) => (
                        <li key={i}>{message}</li>
                    ))}
                </ul>
            )}
        </Card>
    );
}
