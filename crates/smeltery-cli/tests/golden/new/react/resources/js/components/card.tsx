import type { ReactNode } from 'react';

/** A card with a title: `<Card title="…">body</Card>`. */
export default function Card({ title, children }: { title: string; children: ReactNode }) {
    return (
        <section className="panel text-sm leading-relaxed text-stone-700 dark:text-stone-300 [&_code]:rounded [&_code]:bg-ash-100 [&_code]:px-1 [&_code]:py-0.5 [&_code]:font-mono [&_code]:text-xs [&_code]:text-forge-900 dark:[&_code]:bg-forge-800 dark:[&_code]:text-gold-300">
            <h3 className="mb-2 text-base font-semibold text-forge-900 dark:text-ash-100">{title}</h3>
            {children}
        </section>
    );
}
