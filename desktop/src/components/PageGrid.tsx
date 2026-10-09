// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import type { ReactNode } from 'react';

export interface PageGridProps {
  /** `main-side`: the design's mainCols for the Target screen, a column of cards on each side. */
  layout: 'main-side';
  children: ReactNode;
}

/** The columns a page's cards sit in; the stylesheet reads the layout. */
export function PageGrid({ layout, children }: PageGridProps) {
  return (
    <div className="page-grid" data-layout={layout}>
      {children}
    </div>
  );
}
