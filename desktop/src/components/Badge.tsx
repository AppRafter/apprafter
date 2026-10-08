// SPDX-License-Identifier: FSL-1.1-Apache-2.0

export interface BadgeProps {
  count: number;
  /** warn: tab and nav approvals; accent: the bell's unread count. */
  tone?: 'warn' | 'accent';
  /** What is counted, read after the number: "3 approvals waiting". */
  label: string;
}

/** A count; nothing at all at zero. A screen reader hears the sentence, not the bare digit. */
export function Badge({ count, tone = 'warn', label }: BadgeProps) {
  if (count <= 0) return null;
  return (
    <span className="badge" data-tone={tone}>
      <span aria-hidden="true">{count > 99 ? '99+' : count}</span>
      <span className="sr-only">{`${count} ${label}`}</span>
    </span>
  );
}
