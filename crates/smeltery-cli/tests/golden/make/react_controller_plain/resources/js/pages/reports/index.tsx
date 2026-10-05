import { Head } from '@inertiajs/react';

/** `GET /reports` (`index` in `app/controllers/reports.rs`). */
export default function Index() {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Reports" />
            <h1 className="text-3xl font-bold tracking-tight">Reports</h1>
            <p className="mt-4 text-stone-700 dark:text-stone-300">
                This page is <code className="font-mono text-sm">resources/js/pages/reports/index.tsx</code>.
            </p>
        </main>
    );
}
