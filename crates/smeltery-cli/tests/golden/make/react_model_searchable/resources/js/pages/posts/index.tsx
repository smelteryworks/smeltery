import { Head, Link, router } from '@inertiajs/react';
import { useEffect, useRef, useState } from 'react';

import type { Post } from '@/types/post';

/** One part of a highlighted text: rendered as text, the matched parts inside `<mark>` (never as HTML). */
interface Segment {
    text: string;
    matched: boolean;
}

/** A search result: the record, its relevance (`null` without search terms) and its highlighted columns. */
type Hit = Post & { _score: number | null; _highlights: Partial<Record<string, Segment[]>> };

/** One page of results (`smeltery::db::Page`). */
interface Results {
    items: Hit[];
    page: number;
    per_page: number;
    total: number;
    last_page: number;
}

/** `GET /posts?q=…&page=…` (`index` in `app/controllers/posts.rs`): the posts matching the search, ranked. */
export default function Index({ posts, q }: { posts: Results; q: string }) {
    const [search, setSearch] = useState(q);
    const typed = useRef(false);

    // Search as the visitor types, 300 ms after the last key; the page and its scroll position stay.
    useEffect(() => {
        if (!typed.current) {
            return;
        }
        const timer = setTimeout(() => {
            router.get('/posts', search ? { q: search } : {}, { preserveState: true, preserveScroll: true, replace: true });
        }, 300);
        return () => clearTimeout(timer);
    }, [search]);

    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Posts" />
            <div className="flex flex-wrap items-center justify-between gap-4">
                <h1 className="text-3xl font-bold tracking-tight">Posts</h1>
                <Link href="/posts/create" className="btn-primary">
                    New post
                </Link>
            </div>
            <input
                type="search"
                value={search}
                onChange={(e) => {
                    typed.current = true;
                    setSearch(e.target.value);
                }}
                placeholder="Search posts"
                aria-label="Search posts"
                className="form-input mt-8 w-full"
            />
            <ul className="panel mt-4 divide-y divide-ash-200 p-0 dark:divide-forge-800">
                {posts.items.map((post) => (
                    <li key={post.id}>
                        <Link
                            href={`/posts/${post.id}`}
                            className="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800"
                        >
                            {(post._highlights.title ?? [{ text: post.title, matched: false }]).map((part, i) =>
                                part.matched ? <mark key={i}>{part.text}</mark> : <span key={i}>{part.text}</span>,
                            )}
                        </Link>
                    </li>
                ))}
                {posts.items.length === 0 && (
                    <li className="px-6 py-4 text-stone-600 dark:text-stone-400">{q ? 'No posts match your search.' : 'No posts yet.'}</li>
                )}
            </ul>
            {posts.last_page > 1 && (
                <nav className="mt-6 flex items-center justify-between gap-4 text-sm" aria-label="Pages">
                    {posts.page > 1 ? (
                        <Link href="/posts" data={{ q, page: posts.page - 1 }} preserveState className="btn-secondary">
                            Previous
                        </Link>
                    ) : (
                        <span />
                    )}
                    <span className="text-stone-600 dark:text-stone-400">
                        Page {posts.page} of {posts.last_page}
                    </span>
                    {posts.page < posts.last_page ? (
                        <Link href="/posts" data={{ q, page: posts.page + 1 }} preserveState className="btn-secondary">
                            Next
                        </Link>
                    ) : (
                        <span />
                    )}
                </nav>
            )}
        </main>
    );
}
