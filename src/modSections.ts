import type { PackManifest } from "./store";

export interface Section {
  id: string;
  title: string;
  version: string | null;
  isActive: boolean;
  mods: any[];
  enabledCount: number;
  total: number;
}

export const LOOSE_ID = "loose";

export function sectionMods(
  mods: any[],
  packs: PackManifest[],
  activePackId: string | null,
  matches: (mod: any) => boolean,
): { sections: Section[]; enabledElsewhere: Map<number, Section> } {
  const byName = (a: any, b: any) => a.name.localeCompare(b.name);
  const known = new Set(packs.map((p) => p.pack_id));

  const make = (id: string, title: string, version: string | null, members: any[]): Section => ({
    id,
    title,
    version,
    isActive: id !== LOOSE_ID && id === activePackId,
    mods: members.filter(matches).sort(byName),
    enabledCount: members.filter((m) => m.enabled).length,
    total: members.length,
  });

  const packSections = packs.map((p) =>
    make(p.pack_id, p.name, p.version, mods.filter((m) => m.pack?.pack_id === p.pack_id)),
  );
  packSections.sort((a, b) => {
    if (a.isActive !== b.isActive) return a.isActive ? -1 : 1;
    return a.title.localeCompare(b.title);
  });

  const loose = make(
    LOOSE_ID,
    "Hand installed",
    null,
    mods.filter((m) => !m.pack || !known.has(m.pack.pack_id)),
  );

  const all = [...packSections];
  if (loose.total > 0 || packs.length === 0) all.push(loose);

  const enabledElsewhere = new Map<number, Section>();
  for (const section of all) {
    const ids = new Set<string>();
    for (const m of mods) {
      const inSection = section.id === LOOSE_ID
        ? !m.pack || !known.has(m.pack.pack_id)
        : m.pack?.pack_id === section.id;
      if (inSection && m.enabled && m.nexus_mod_id != null) ids.add(String(m.nexus_mod_id));
    }
    ids.forEach((id) => enabledElsewhere.set(Number(id), section));
  }

  return { sections: all, enabledElsewhere };
}
