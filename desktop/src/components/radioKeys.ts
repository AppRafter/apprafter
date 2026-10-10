// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Home and End for a group of native radios (choice cards, chips, a radio list, a table's rows).
// The engine already moves the choice with the arrow keys and skips a disabled radio, the same in
// WebKitGTK, WKWebView and WebView2, but no engine maps Home and End for radios; SegmentedControl
// does, and these groups follow it. Put on the element that holds the group.
import type { KeyboardEvent } from 'react';

/** Home or End pressed on a radio chooses and focuses the first or last radio of its group that can be chosen. */
export function onRadioHomeEnd(event: KeyboardEvent<HTMLElement>): void {
  if (event.key !== 'Home' && event.key !== 'End') return;
  if (event.altKey || event.ctrlKey || event.metaKey || event.shiftKey) return;
  const from = event.target;
  if (!(from instanceof HTMLInputElement) || from.type !== 'radio') return;
  // By the name attribute, not a selector: React's useId names hold characters a selector must escape.
  const group = [
    ...event.currentTarget.querySelectorAll<HTMLInputElement>('input[type="radio"]'),
  ].filter((radio) => radio.name === from.name && !radio.disabled);
  const to = event.key === 'Home' ? group[0] : group.at(-1);
  if (to === undefined) return;
  event.preventDefault();
  to.focus();
  // A click checks it and fires its change, as a click with the pointer would.
  if (!to.checked) to.click();
}
