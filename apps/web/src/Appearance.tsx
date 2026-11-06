// Visual adaptation of ikenxuan/karin-plugin-kkk's DouyinVideoWork/AmbientCover.
// SPDX-License-Identifier: GPL-3.0-only
// Pinned sources and attribution: ../vendor/karin-plugin-kkk/source.json
import { useEffect, useState } from "react";

export function useAppearance() {
  const [theme, setTheme] = useState<"light" | "dark">(() => {
    try {
      return localStorage.getItem("qscope.appearance") === "dark"
        ? "dark"
        : "light";
    } catch {
      return "light";
    }
  });
  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    const meta = document.querySelector('meta[name="theme-color"]');
    meta?.setAttribute("content", theme === "light" ? "#f7f7f9" : "#111116");
    try {
      localStorage.setItem("qscope.appearance", theme);
    } catch {
      /* Private browser storage may be disabled. */
    }
  }, [theme]);
  return {
    theme,
    toggleTheme: () =>
      setTheme((previous) => (previous === "light" ? "dark" : "light")),
  };
}

export function PosterBackdrop() {
  return (
    <div className="poster-backdrop" aria-hidden="true">
      <div className="ambient-color ambient-lilac" />
      <div className="ambient-color ambient-rose" />
      <div className="ambient-color ambient-blue" />
      <div className="ambient-overlay" />
      <svg className="poster-grain" xmlns="http://www.w3.org/2000/svg">
        <defs>
          <filter id="qscope-poster-noise">
            <feTurbulence
              type="fractalNoise"
              baseFrequency="0.8"
              numOctaves="2"
              stitchTiles="stitch"
            />
            <feColorMatrix type="saturate" values="0" />
            <feComponentTransfer>
              <feFuncR type="discrete" tableValues="0 1" />
              <feFuncG type="discrete" tableValues="0 1" />
              <feFuncB type="discrete" tableValues="0 1" />
              <feFuncA type="linear" slope="2" intercept="-0.5" />
            </feComponentTransfer>
          </filter>
        </defs>
        <rect
          width="100%"
          height="100%"
          filter="url(#qscope-poster-noise)"
          fill="white"
        />
      </svg>
    </div>
  );
}
