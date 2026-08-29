import type { ReactNode } from "react";

export interface CitationProps {
  file: string;
  line: number;
}

export const Citation = ({ file, line }: CitationProps): ReactNode => (
  <span className="citation">
    {file}:{line}
  </span>
);

export function Empty() {
  return <div />;
}
