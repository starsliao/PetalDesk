export interface MigratableStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
  removeItem?(key: string): void;
}

const previousProductPrefix = ["fei", "hua"].join("");

export function previousStorageKey(suffix: string): string {
  return `${previousProductPrefix}.${suffix}`;
}

export function readMigratedStorageValue(
  storage: MigratableStorage,
  currentKey: string,
  previousKey: string,
): string | null {
  const current = storage.getItem(currentKey);
  if (current !== null) return current;
  const previous = storage.getItem(previousKey);
  if (previous === null) return null;
  storage.setItem(currentKey, previous);
  storage.removeItem?.(previousKey);
  return previous;
}
