import type { KeyValueStorage } from "@puppet-master/client-core/platform";

/** The async subset of expo-secure-store the storage adapter drives. */
export interface AsyncStringStore {
  getItemAsync(key: string): Promise<string | null>;
  setItemAsync(key: string, value: string): Promise<void>;
}

/**
 * Sync KeyValueStorage over the async OS secure store: reads come from an
 * in-memory cache hydrated once at startup and writes flush through serially
 * so the stored order matches the observed order. A failed flush keeps the
 * cached value so the running app stays consistent.
 */
export class CachedSecureStorage implements KeyValueStorage {
  private cache = new Map<string, string>();
  private queue: Promise<void> = Promise.resolve();

  private constructor(
    private backend: AsyncStringStore,
    private onError: ((key: string, err: unknown) => void) | undefined,
  ) {}

  static async hydrate(
    backend: AsyncStringStore,
    keys: readonly string[],
    onError?: (key: string, err: unknown) => void,
  ): Promise<CachedSecureStorage> {
    const storage = new CachedSecureStorage(backend, onError);
    for (const key of keys) {
      try {
        const value = await backend.getItemAsync(key);
        if (value !== null) storage.cache.set(key, value);
      } catch (err) {
        onError?.(key, err);
      }
    }
    return storage;
  }

  getItem(key: string): string | null {
    return this.cache.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.cache.set(key, value);
    this.queue = this.queue
      .then(() => this.backend.setItemAsync(key, value))
      .catch((err: unknown) => this.onError?.(key, err));
  }

  /** Resolves once every write issued so far has settled. */
  flush(): Promise<void> {
    return this.queue;
  }
}
