import { Head, Link } from '@inertiajs/react';
import type { ReactNode } from 'react';

const tabs = [
    { href: '/settings/profile', label: 'Profile' },
    { href: '/settings/password', label: 'Password' },
    { href: '/settings/two-factor', label: 'Two-factor authentication' },
];

/** The settings pages: a title, the tabs and the page's panel. */
export default function SettingsLayout({ title, current, children }: { title: string; current: string; children: ReactNode }) {
    return (
        <main className="mx-auto max-w-xl px-4 py-12 sm:px-6">
            <Head title={title} />
            <p className="mono-label">Settings</p>
            <h1 className="mt-2 text-3xl font-bold tracking-tight">{title}</h1>
            <nav className="mt-6 flex flex-wrap gap-2 text-sm" aria-label="Settings">
                {tabs.map((tab) => (
                    <Link
                        key={tab.href}
                        href={tab.href}
                        className="rounded-lg px-3 py-2 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-900"
                        aria-current={tab.href === current ? 'page' : undefined}
                    >
                        {tab.label}
                    </Link>
                ))}
            </nav>
            {children}
        </main>
    );
}
