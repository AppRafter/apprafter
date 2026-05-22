// AppRafter logo — stepped platform, two-tone (slate + teal accent).
// Variants: twoTone (default), mono. Inherits font color via currentColor.

const LogoMark = ({ size = 26, variant = "twoTone" }) => {
  const baseFill = "currentColor";
  const accentFill = variant === "mono" ? "currentColor" : "var(--accent)";
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 200 200"
      width={size}
      height={size}
      shapeRendering="geometricPrecision"
      aria-label="AppRafter"
      style={{ display: "block", flexShrink: 0 }}
    >
      <g transform="translate(0 -5)">
        <path
          d="M 100.000 126.192 L 180.000 131.786 L 180.000 143.786 L 100.000 149.380 L 20.000 143.786 L 20.000 131.786 Z M 55.789 146.288 L 63.090 128.773 L 72.018 128.148 L 64.211 146.877 Z M 136.910 128.773 L 144.211 146.288 L 135.789 146.877 L 127.982 128.148 Z M 99.500 149.345 L 99.500 126.227 L 100.000 126.192 L 100.000 149.380 Z M 100.000 149.380 L 100.000 126.192 L 100.500 126.227 L 100.500 149.345 Z"
          fill={baseFill}
          fillRule="evenodd"
        />
        <path
          d="M 100.000 97.500 L 170.445 113.118 L 171.284 125.118 L 100.000 109.314 L 28.716 125.118 L 29.555 113.118 Z M 68.269 116.349 L 73.694 103.332 L 83.244 101.215 L 77.818 114.232 Z M 126.306 103.332 L 131.731 116.349 L 122.182 114.232 L 116.756 101.215 Z M 99.500 109.425 L 99.500 97.611 L 100.000 97.500 L 100.000 109.314 Z M 100.000 109.314 L 100.000 97.500 L 100.500 97.611 L 100.500 109.425 Z"
          fill={baseFill}
          fillRule="evenodd"
        />
        <path
          d="M 100.000 74.454 L 160.052 92.814 L 160.892 104.814 L 100.000 86.197 L 39.108 104.814 L 39.948 92.814 Z M 78.038 92.912 L 83.648 79.453 L 93.581 76.416 L 87.971 89.875 Z M 116.352 79.453 L 121.962 92.912 L 112.029 89.875 L 106.419 76.416 Z M 99.500 86.350 L 99.500 74.607 L 100.000 74.454 L 100.000 86.197 Z M 100.000 86.197 L 100.000 74.454 L 100.500 74.607 L 100.500 86.350 Z"
          fill={baseFill}
          fillRule="evenodd"
        />
        <path
          d="M 50.870 71.474 L 94.735 52.855 L 88.838 67.002 L 50.031 83.474 Z M 105.265 52.855 L 149.130 71.474 L 149.969 83.474 L 111.162 67.002 Z"
          fill={accentFill}
          fillRule="evenodd"
        />
      </g>
    </svg>
  );
};

const Wordmark = ({ variant = "twoTone" }) => {
  if (variant === "mono") {
    return <span className="wordmark">AppRafter</span>;
  }
  return (
    <span className="wordmark">
      App<span className="accented">Rafter</span>
    </span>
  );
};

const Brand = ({ variant = "twoTone", size = 26 }) => (
  <a href="#top" className="brand" aria-label="AppRafter home">
    <LogoMark size={size} variant={variant} />
    <Wordmark variant={variant} />
  </a>
);

Object.assign(window, { LogoMark, Wordmark, Brand });
