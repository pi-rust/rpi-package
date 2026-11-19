import logoUrl from "./assets/rpi-logo.png";

/**
 * Project logo from the main repo README, bundled locally; no remote request.
 *
 * The PNG is imported (not referenced from `public/`) so Vite emits it under
 * `/assets/` — the only static route the extension server exposes. A `public/`
 * root-level file renders in `vite dev`/`preview` but 404s in the real
 * extension because the server only serves `/`, `/assets/*` and `/favicon.svg`.
 */
export function BrandLogo({ size = 28 }: { size?: number }) {
  return (
    <img
      className="rpi-logo"
      src={logoUrl}
      width={size}
      height={size}
      alt=""
      aria-hidden="true"
    />
  );
}
