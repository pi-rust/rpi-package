/** Project logo from the main repo README, bundled locally; no remote request. */
export function BrandLogo({ size = 28 }: { size?: number }) {
  return (
    <img
      className="rpi-logo"
      src="/rpi-logo.png"
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
    />
  );
}
