import { SECTIONS } from "./sections";

export const ALL_IDS: string[] = SECTIONS.map((s) => s.id);

/** The parts that are open at first: the one named in the address (`#google`), if it exists. */
export function initialOpen(hash?: string): Set<string> {
  return new Set(hash && ALL_IDS.includes(hash) ? [hash] : []);
}

/** Open or close one part. */
export function setOpen(open: Set<string>, id: string, isOpen: boolean): Set<string> {
  const next = new Set(open);
  if (isOpen) next.add(id);
  else next.delete(id);
  return next;
}
