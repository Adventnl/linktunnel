import type { CSSProperties } from "react";

const paths = {
  popout: "M14 3h7v7 M21 3l-11 11 M10 3H3v18h18v-7",
  monitor: "M3 4h18v12H3z M8 21h8 M12 16v5",
  laptop: "M5 3h14v13H5z M2 20h20l-3-4H5z",
  terminal: "m5 6 5 5-5 5 M13 17h6",
  plus: "M12 5v14 M5 12h14",
  settings:
    "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8 M9 3l-1 3-3 1-2 5 2 5 3 1 1 3h6l1-3 3-1 2-5-2-5-3-1-1-3z",
  phone: "M7 2h10v20H7z M10 18h4",
  more: "M5 12h.01 M12 12h.01 M19 12h.01",
  chevron: "m9 5 7 7-7 7",
  sidebar: "M3 3h18v18H3z M9 3v18",
  expand: "M8 3H3v5 M16 3h5v5 M3 16v5h5 M21 16v5h-5",
  close: "m6 6 12 12 M6 18 18 6",
  minus: "M5 12h14",
  square: "M5 5h14v14H5z",
  lock: "M5 10h14v11H5z M8 10V6a4 4 0 0 1 8 0v4",
  shield: "m12 2 8 3v6c0 5-4 9-8 11-4-2-8-6-8-11V5z m-4 9 3 3 5-5",
  power: "M12 2v9 M6 5a9 9 0 1 0 12 0",
  trash: "M3 6h18 M9 6V3h6v3 M6 6l1 15h10l1-15 M10 10v7 M14 10v7",
  command:
    "M8 8h8v8H8z M8 8H5a3 3 0 1 1 3-3v3 M16 8V5a3 3 0 1 1 3 3h-3 M16 16h3a3 3 0 1 1-3 3v-3 M8 16v3a3 3 0 1 1-3-3h3",
  link: "m9 15 6-6 M8 16l-1 1a4 4 0 0 1-6-6l4-4a4 4 0 0 1 6 0 M16 8l1-1a4 4 0 0 1 6 6l-4 4a4 4 0 0 1-6 0",
  moon: "M20 15A9 9 0 0 1 9 4a9 9 0 1 0 11 11",
  refresh: "M20 8a9 9 0 1 0 1 8 M20 3v5h-5",
  check: "m5 12 4 4L19 6",
  arrow: "M4 12h16 m-6-6 6 6-6 6",
} as const;
export type IconName = keyof typeof paths;
export function Icon({
  name,
  size = 18,
  style,
}: {
  name: IconName;
  size?: number;
  style?: CSSProperties;
}) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.6"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      style={style}
    >
      <path d={paths[name]} />
    </svg>
  );
}
