/** The brand mark (public/logo.png, made from logo_no_background.png at the repo root). */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <img
      src="/logo.png"
      alt=""
      aria-hidden="true"
      width={size}
      height={size}
      className="ui-logo shrink-0 rounded-md object-contain"
      style={{ width: size, height: size }}
    />
  );
}
