/** Light variant of the official rpi mark, bundled locally; no remote request. */
export function BrandLogo({ size = 28 }: { size?: number }) {
  return (
    <img
      className="rpi-logo"
      src="/favicon.svg"
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
    />
  );
}
