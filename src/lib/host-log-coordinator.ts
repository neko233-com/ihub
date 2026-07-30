export type HostLogReadOutcome<T> =
  | { status: "applied"; snapshot: T }
  | { status: "blocked" }
  | { status: "stale" };

export type HostLogForegroundAction = "refresh" | "copy" | "clear";

export type HostLogForegroundContext<T> = {
  isCurrent: () => boolean;
  read: () => Promise<HostLogReadOutcome<T>>;
};

export type HostLogForegroundTask<T> =
  | { started: false }
  | { started: true; promise: Promise<T> };

type ReadFlight<T> = Promise<
  | { ok: true; generation: number; snapshot: T }
  | { ok: false; generation: number; error: unknown }
>;

/**
 * Serializes the settings log reader without coupling the concurrency rules to
 * React render timing. A session is the lifetime of one open settings surface.
 */
export class HostLogCoordinator<T> {
  private active = false;
  private generation = 0;
  private foregroundAction: {
    action: HostLogForegroundAction;
    token: symbol;
  } | null = null;
  private readFlight: ReadFlight<T> | null = null;

  constructor(
    private readonly readSnapshot: () => Promise<T>,
    private readonly clearSnapshots: (current?: T) => Promise<T>,
  ) {}

  openSession(): void {
    this.active = true;
    this.generation += 1;
  }

  closeSession(): void {
    this.active = false;
    this.generation += 1;
  }

  readInBackground(): Promise<HostLogReadOutcome<T>> {
    if (this.foregroundAction !== null) {
      return Promise.resolve({ status: "blocked" });
    }
    return this.readForCurrentSession();
  }

  startForeground<R>(
    action: Exclude<HostLogForegroundAction, "clear">,
    operation: (context: HostLogForegroundContext<T>) => Promise<R>,
  ): HostLogForegroundTask<R> {
    if (!this.active || this.foregroundAction !== null) {
      return { started: false };
    }

    const token = Symbol(action);
    const generation = this.generation;
    this.foregroundAction = { action, token };
    const promise = (async () => operation({
      isCurrent: () =>
        this.active
        && this.generation === generation
        && this.foregroundAction?.token === token,
      read: () => this.readForForeground(token),
    }))().finally(() => {
      if (this.foregroundAction?.token === token) {
        this.foregroundAction = null;
      }
    });
    return { started: true, promise };
  }

  startClear(current?: T): HostLogForegroundTask<HostLogReadOutcome<T>> {
    if (!this.active || this.foregroundAction !== null) {
      return { started: false };
    }

    const token = Symbol("clear");
    this.foregroundAction = { action: "clear", token };
    // Invalidate an older read synchronously, before the async clear task can
    // yield. Its response may settle, but can no longer publish a snapshot.
    const clearGeneration = ++this.generation;
    const olderRead = this.readFlight;
    const promise = (async (): Promise<HostLogReadOutcome<T>> => {
      if (olderRead) {
        await olderRead;
      }

      const result = await this.clearSnapshots(current).then(
        (snapshot) => ({ ok: true as const, snapshot }),
        (error) => ({ ok: false as const, error }),
      );
      if (
        !this.active
        || this.generation !== clearGeneration
        || this.foregroundAction?.token !== token
      ) {
        return { status: "stale" };
      }
      if (!result.ok) {
        throw result.error;
      }
      return { status: "applied", snapshot: result.snapshot };
    })().finally(() => {
      if (this.foregroundAction?.token === token) {
        this.foregroundAction = null;
      }
    });
    return { started: true, promise };
  }

  private readForForeground(token: symbol): Promise<HostLogReadOutcome<T>> {
    if (this.foregroundAction?.token !== token) {
      return Promise.resolve({ status: "blocked" });
    }
    return this.readForCurrentSession();
  }

  private async readForCurrentSession(): Promise<HostLogReadOutcome<T>> {
    if (!this.active) {
      return { status: "stale" };
    }
    const requestedGeneration = this.generation;

    let flight = this.readFlight;
    if (!flight) {
      const generation = this.generation;
      flight = this.readSnapshot().then(
        (snapshot) => ({ ok: true as const, generation, snapshot }),
        (error) => ({ ok: false as const, generation, error }),
      );
      this.readFlight = flight;
      void flight.finally(() => {
        if (this.readFlight === flight) {
          this.readFlight = null;
        }
      });
    }

    const result = await flight;
    if (!this.active || requestedGeneration !== this.generation) {
      return { status: "stale" };
    }
    if (result.generation !== requestedGeneration) {
      // A new settings session can briefly encounter the previous session's
      // draining flight (notably under React StrictMode). Wait for it rather
      // than overlap native reads, then immediately obtain a current snapshot.
      if (this.readFlight === flight) {
        this.readFlight = null;
      }
      return this.readForCurrentSession();
    }
    if (!result.ok) {
      throw result.error;
    }
    return { status: "applied", snapshot: result.snapshot };
  }
}
