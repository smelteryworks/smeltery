import type { CSSProperties } from 'react';

/** Where each spark rises: horizontal position, duration, delay and sideways drift. */
const SPARKS: [string, string, string, string][] = [
    ['30%', '3.2s', '0s', '-18px'],
    ['42%', '2.6s', '0.8s', '12px'],
    ['50%', '3.6s', '1.6s', '-6px'],
    ['58%', '2.9s', '0.4s', '20px'],
    ['66%', '3.4s', '2.1s', '-12px'],
    ['36%', '4s', '2.7s', '24px'],
    ['62%', '3.8s', '1.2s', '-22px'],
];

/** The welcome page's forge: an isometric ingot over an ember glow, with sparks rising in CSS (`.spark`). */
export default function Ingot() {
    return (
        <div className="relative mx-auto aspect-square w-full max-w-60 sm:max-w-sm" aria-hidden="true">
            <div className="forge-glow absolute inset-6 rounded-full"></div>
            <svg className="absolute inset-0 m-auto w-3/5 drop-shadow-[0_0_30px_rgb(255_122_31/0.55)]" viewBox="0 0 120 100">
                <defs>
                    <linearGradient id="ingot-top" x1="0" y1="0" x2="1" y2="1">
                        <stop offset="0" stopColor="#ffe2a8" />
                        <stop offset="1" stopColor="#ffb84d" />
                    </linearGradient>
                    <linearGradient id="ingot-left" x1="0" y1="0" x2="0" y2="1">
                        <stop offset="0" stopColor="#ff7a52" />
                        <stop offset="1" stopColor="#c72e16" />
                    </linearGradient>
                    <linearGradient id="ingot-right" x1="0" y1="0" x2="0" y2="1">
                        <stop offset="0" stopColor="#e8361d" />
                        <stop offset="1" stopColor="#7a1c0b" />
                    </linearGradient>
                </defs>
                <path d="M60 20 104 40 60 60 16 40z" fill="url(#ingot-top)" />
                <path d="M16 40 60 60v24L16 64z" fill="url(#ingot-left)" />
                <path d="M104 40 60 60v24l44-20z" fill="url(#ingot-right)" />
            </svg>
            {SPARKS.map(([x, t, d, drift]) => (
                <span key={x + d} className="spark" style={{ '--x': x, '--t': t, '--d': d, '--drift': drift } as CSSProperties}></span>
            ))}
        </div>
    );
}
