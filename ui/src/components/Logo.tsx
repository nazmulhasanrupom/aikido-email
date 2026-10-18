/** The brand mark (public/logo.jpeg, made from logo.jpeg at the repo root). */
export function Logo({ size = 28 }: { size?: number }) {
  return (
    <img
      src="/logo.jpeg"
      alt=""
      aria-hidden="true"
      width={size}
      height={size}
      className="shrink-0 rounded-md bg-white object-contain ring-1 ring-black/5"
      style={{ width: size, height: size }}
    />
  );
}
