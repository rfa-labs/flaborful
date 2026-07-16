/** @type {import('tailwindcss').Config} */
export default {
  content: ['./src/**/*.{html,js,svelte,ts}'],
  theme: {
    extend: {
      colors: {
        // Brand ramp, violet-forward. Violet (#7c4dff) is the 500/600 anchor so every existing
        // `brand-600`/`brand-700` usage across the app becomes the new primary automatically —
        // this is the same accent as the landing page's --violet.
        brand: {
          50:  '#f2ecff',
          100: '#e6dbff',
          200: '#cebaff',
          300: '#b193ff',
          400: '#9670ff',
          500: '#7c4dff',
          600: '#6a34ff',
          700: '#5a27e0',
          800: '#4a20b8',
          900: '#3a1a8f',
        },
        // Warm-neutral scale. Migration target for the app's hardcoded cool `gray-*` utilities —
        // derived off the landing page's --cream (#fdf8f3) and --ink (#1c1524) so the whole app
        // sits on the same warm ground. Named `sand` to read clearly at call sites.
        sand: {
          50:  '#fdf8f3',
          100: '#f7efe8',
          200: '#ece0d6',
          300: '#dccabc',
          400: '#b4a396',
          500: '#8a7d72',
          600: '#655a51',
          700: '#4a4139',
          800: '#302a25',
          900: '#1c1524',
        },
        // NB: deliberately NOT overriding Tailwind's built-in amber/rose/teal — the app already
        // uses shades of them (amber-50/200/700 in status banners, etc.) and a partial override
        // would drop those. The landing page defines its own --amber/--rose/--teal locally.
      },
      fontFamily: {
        sans: ['Public Sans', 'system-ui', 'sans-serif'],
        display: ['Bricolage Grotesque', 'system-ui', 'sans-serif'],
        mono: ['JetBrains Mono', 'Fira Code', 'monospace'],
      },
    },
  },
  plugins: [require('@tailwindcss/typography')],
};
