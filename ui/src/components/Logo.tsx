export function Logo({ size = 28 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 32 32" aria-hidden="true">
      <rect width="32" height="32" rx="8" fill="var(--accent)" />
      <path d="M8 22 16 9l8 13h-4.2L16 15.6 12.2 22z" fill="var(--accent-fg)" />
    </svg>
  );
}
