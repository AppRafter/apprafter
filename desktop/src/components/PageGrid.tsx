// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';

export interface PageGridProps {
  /** The main column's cards. */
  main: ReactNode;
  /**
   * The side column's cards. Only with them is the page two columns (the design's mainCols, the
   * side the wider track); without, the main column takes the page's width.
   */
  side?: ReactNode;
}

/**
 * The columns a page's cards sit in. The layout follows the content: a side column that would
 * hold nothing never squeezes the main cards into the narrow track.
 */
export function PageGrid({ main, side }: PageGridProps) {
  if (side === undefined || side === null) {
    return (
      <div className="page-grid" data-layout="single">
        {main}
      </div>
    );
  }
  return (
    <div className="page-grid" data-layout="main-side">
      <div className="page-grid-column">{main}</div>
      <div className="page-grid-column">{side}</div>
    </div>
  );
}
