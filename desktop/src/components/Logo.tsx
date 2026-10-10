// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The AppRafter mark (the design's title-bar SVG): three rafters in --fg under an --accent
// ridge. Colours come from shell.css classes, so the mark follows the theme.

export function Logo({ size = 18 }: { size?: number }) {
  return (
    <svg
      className="logo"
      viewBox="12 13 176 176"
      width={size}
      height={size}
      aria-hidden="true"
      focusable="false"
    >
      <g fillRule="evenodd">
        <path
          className="logo-fg"
          d="M 100.000 126.192 L 180.000 131.786 L 180.000 143.786 L 100.000 149.380 L 20.000 143.786 L 20.000 131.786 Z M 55.789 146.288 L 63.090 128.773 L 72.018 128.148 L 64.211 146.877 Z M 136.910 128.773 L 144.211 146.288 L 135.789 146.877 L 127.982 128.148 Z"
        />
        <path
          className="logo-fg"
          d="M 100.000 97.500 L 170.445 113.118 L 171.284 125.118 L 100.000 109.314 L 28.716 125.118 L 29.555 113.118 Z M 68.269 116.349 L 73.694 103.332 L 83.244 101.215 L 77.818 114.232 Z M 126.306 103.332 L 131.731 116.349 L 122.182 114.232 L 116.756 101.215 Z"
        />
        <path
          className="logo-fg"
          d="M 100.000 74.454 L 160.052 92.814 L 160.892 104.814 L 100.000 86.197 L 39.108 104.814 L 39.948 92.814 Z M 78.038 92.912 L 83.648 79.453 L 93.581 76.416 L 87.971 89.875 Z M 116.352 79.453 L 121.962 92.912 L 112.029 89.875 L 106.419 76.416 Z"
        />
        <path
          className="logo-accent"
          d="M 50.870 71.474 L 94.735 52.855 L 88.838 67.002 L 50.031 83.474 Z M 105.265 52.855 L 149.130 71.474 L 149.969 83.474 L 111.162 67.002 Z"
        />
      </g>
    </svg>
  );
}
