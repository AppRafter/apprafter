// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { type ReactNode, useId } from 'react';

export interface CardProps {
  /** The eyebrow title; it names the region. */
  title: string;
  /** Mono text at the right of the title. */
  note?: ReactNode;
  /** `danger`: the border and the title in --err (the Danger zone). */
  tone?: 'default' | 'danger';
  /** Its rows, usually CardRows. */
  children: ReactNode;
}

/** The design's card(): a titled surface of rows, named for a screen reader by its title. */
export function Card({ title, note, tone = 'default', children }: CardProps) {
  const id = useId();
  return (
    <section className="card" data-tone={tone} aria-labelledby={id}>
      <header className="card-head">
        <h2 className="card-title" id={id}>
          {title}
        </h2>
        {note !== undefined && <p className="card-note">{note}</p>}
      </header>
      {children}
    </section>
  );
}
