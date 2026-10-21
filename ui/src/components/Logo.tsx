/** The brand mark: a red tile with an A and a paper plane (public/logo.svg, a copy of logo.svg at the repo root). */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <img
      src="/logo.svg"
      alt=""
      aria-hidden="true"
      width={size}
      height={size}
      className="ui-logo shrink-0"
      style={{ width: size, height: size }}
    />
  );
}
