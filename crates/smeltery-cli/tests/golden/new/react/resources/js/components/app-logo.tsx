/** The logo mark: a molten ingot seen from above. */
export default function AppLogo() {
    return (
        <svg className="h-7 w-7" viewBox="0 0 32 32" aria-hidden="true">
            <defs>
                <linearGradient id="logo-molten" x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0" stopColor="#ffd27a" />
                    <stop offset=".5" stopColor="#ff4a1c" />
                    <stop offset="1" stopColor="#a3260f" />
                </linearGradient>
            </defs>
            <path d="M16 4 28 10v12l-12 6-12-6V10z" fill="url(#logo-molten)" />
            <path d="M16 4 28 10l-12 6-12-6z" fill="#ffd27a" opacity=".55" />
        </svg>
    );
}
