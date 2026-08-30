/**
 * Icons. One style: a 16px grid, stroke-only, 1.5 stroke weight, currentColor,
 * round caps and joins. No emoji anywhere in this app (GUI §6 — an emoji is a
 * different typeface at a different weight in a colour you did not choose).
 *
 * The paths are lifted from design/*.dc.html so the app and the mockups draw
 * the same glyph. Every icon-only control gets an accessible name from its
 * caller, never from the icon (GUI §8).
 */

import type { JSX, SVGProps } from "react";

export type IconName =
  | "search"
  | "file"
  | "fileDim"
  | "folder"
  | "ask"
  | "chip"
  | "plus"
  | "pencil"
  | "trash"
  | "activity"
  | "settings"
  | "warning"
  | "chevron"
  | "sidebar"
  | "copy"
  | "close"
  | "arrowRight";

const PATHS: Record<IconName, JSX.Element> = {
  search: (
    <>
      <circle cx="7" cy="7" r="4.5" />
      <path d="M10.5 10.5 14 14" />
    </>
  ),
  file: (
    <>
      <path d="M9 1.5H4a1.5 1.5 0 0 0-1.5 1.5v10A1.5 1.5 0 0 0 4 14.5h8a1.5 1.5 0 0 0 1.5-1.5V6z" />
      <path d="M9 1.5V6h4.5" />
    </>
  ),
  /* A file the index holds by name and date only: the page corner is there,
     the ruled lines that stand for readable content are not. */
  fileDim: (
    <>
      <path d="M9 1.5H4a1.5 1.5 0 0 0-1.5 1.5v10A1.5 1.5 0 0 0 4 14.5h8a1.5 1.5 0 0 0 1.5-1.5V6z" strokeDasharray="2.4 2" />
      <path d="M9 1.5V6h4.5" />
    </>
  ),
  folder: (
    <path d="M1.5 4a1 1 0 0 1 1-1h3.2l1.4 1.6h6.4a1 1 0 0 1 1 1v6.9a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1z" />
  ),
  ask: <path d="M13.5 8.5a5 5 0 0 1-5 5H3l1.4-2.2A5 5 0 1 1 13.5 8.5z" />,
  /* Models. A die, not the speech bubble Ask uses — in an icon-only switcher
     two sections drawn with the same glyph are one section drawn twice. */
  chip: (
    <>
      <rect x="4.5" y="4.5" width="7" height="7" rx="1.2" />
      <path d="M6.5 1.75v2.75M9.5 1.75v2.75M6.5 11.5v2.75M9.5 11.5v2.75M1.75 6.5h2.75M1.75 9.5h2.75M11.5 6.5h2.75M11.5 9.5h2.75" />
    </>
  ),
  plus: <path d="M8 3.25v9.5M3.25 8h9.5" />,
  pencil: (
    <>
      <path d="M11.4 2.4a1.6 1.6 0 0 1 2.2 2.2L5.5 12.7l-3 .8.8-3z" />
      <path d="M10.2 3.6l2.2 2.2" />
    </>
  ),
  trash: (
    <>
      <path d="M2.75 4.25h10.5" />
      <path d="M6 4.25V2.9a.9.9 0 0 1 .9-.9h2.2a.9.9 0 0 1 .9.9v1.35" />
      <path d="M4.1 4.25l.6 8.1a1.2 1.2 0 0 0 1.2 1.15h4.2a1.2 1.2 0 0 0 1.2-1.15l.6-8.1" />
    </>
  ),
  activity: <path d="M1.5 8h3l2-4.5L9.5 12l2-4h3" />,
  settings: (
    <>
      <circle cx="8" cy="8" r="2.2" />
      <path d="M8 1.5v1.8M8 12.7v1.8M14.5 8h-1.8M3.3 8H1.5M12.6 3.4l-1.3 1.3M4.7 11.3l-1.3 1.3M12.6 12.6l-1.3-1.3M4.7 4.7 3.4 3.4" />
    </>
  ),
  warning: (
    <>
      <path d="M8 2.5 14.5 13.5h-13z" />
      <path d="M8 6.5v3M8 11.6v.1" />
    </>
  ),
  chevron: <path d="M4 6.5 8 10.5l4-4" />,
  sidebar: (
    <>
      <rect x="1.75" y="2.75" width="12.5" height="10.5" rx="1.5" />
      <path d="M6.25 2.75v10.5" />
    </>
  ),
  copy: (
    <>
      <rect x="5.75" y="5.75" width="8.5" height="8.5" rx="1.5" />
      <path d="M10.25 3.25v-.5a1 1 0 0 0-1-1h-6.5a1 1 0 0 0-1 1v6.5a1 1 0 0 0 1 1h.5" />
    </>
  ),
  close: <path d="M4 4l8 8M12 4l-8 8" />,
  arrowRight: (
    <>
      <path d="M2.5 8h11" />
      <path d="M9.5 4 13.5 8l-4 4" />
    </>
  ),
};

export interface IconProps extends Omit<SVGProps<SVGSVGElement>, "children"> {
  name: IconName;
  size?: number;
}

export function Icon({ name, size = 14, ...rest }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 16 16"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.8}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
      style={{ flexShrink: 0 }}
      {...rest}
    >
      {PATHS[name]}
    </svg>
  );
}
