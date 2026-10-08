<!-- SPDX-License-Identifier: FSL-1.1-Apache-2.0 -->

# AppRafter Desktop design source

This directory holds the Claude Design export that the AppRafter Desktop slices trace to: Claude
Design project `7a6d4d01-c32a-4147-868f-3048ce470201`, file `AppRafterApp.dc.html` plus its
runtime `support.js`. The Claude Design project stays the upstream; this copy is a snapshot of it,
so a slice can cite a stable path in the repository.

The target is layout **variant A**, the default `variant` prop: a section sidebar. Variants B
(top tabs and a status bar) and C (vertical cluster tabs with live metrics) cover only Overview,
Applications and Approvals and are not the target.

`shots/` holds renders of variant A's screens, including `overview-light.png` (the light theme)
and `locked.png` (the lock screen). `variant-b.png` and `variant-c.png` render the two variants
that are not the target and are kept for reference only.

The files are kept verbatim: they are excluded from the SPDX header check, from Biome and from
`bun test`, and nothing here is built or shipped with the app.
