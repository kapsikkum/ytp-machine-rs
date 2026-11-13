// Undo/redo for Studio edits, mirroring crates/ytp-core/src/history.rs: a
// bounded stack of Project snapshots with a cursor naming the current one.

import type { Project } from '#lib/project.ts';

/** Snapshots kept by `new ProjectHistory` unless `setCapacity` says otherwise. */
export const DEFAULT_CAPACITY = 200;

/**
 * A bounded undo/redo stack of project snapshots. A snapshot is a plain JSON
 * copy of a project; `apply` records one and clears anything that could have
 * been redone, `undo` and `redo` move the cursor. Past capacity the oldest
 * snapshot is dropped, so a long session cannot grow without limit. A capacity
 * of 0 or 1 keeps only the current state, i.e. no history.
 */
export class ProjectHistory {
  /** What the caller asked for; 0 means "current state only". */
  private limit: number;
  /** Snapshots, oldest first. Never empty. */
  private states: Project[];
  /** Index in `states` of the snapshot that is current. */
  private cursor: number;

  /** A history holding `project` as its only, current, state. */
  constructor(project: Project, capacity = DEFAULT_CAPACITY) {
    this.limit = normalize(capacity);
    this.states = [structuredClone(project)];
    this.cursor = 0;
  }

  /** The configured capacity; 0 and 1 both mean no history. */
  capacity(): number {
    return this.limit;
  }

  /** How many snapshots are actually kept: at least one. */
  private keep(): number {
    return Math.max(1, this.limit);
  }

  /**
   * Change how many snapshots are kept. Shrinking drops the oldest ones but
   * never drops the current state; 0 is treated as 1.
   */
  setCapacity(capacity: number): void {
    this.limit = normalize(capacity);
    const cap = this.keep();
    if (this.states.length <= cap) return;
    // Drop the oldest states, but stop at the cursor so the current snapshot
    // survives.
    const excess = this.states.length - cap;
    const drop = Math.min(excess, this.cursor);
    this.states.splice(0, drop);
    this.cursor -= drop;
    // If the cursor was near the front, the tail is what is left to trim.
    const left = this.keep();
    if (this.states.length > left) {
      this.states.length = left;
      this.cursor = Math.min(this.cursor, this.states.length - 1);
    }
  }

  /**
   * Record `project` as the newest state and make it current. Anything that
   * could have been redone is discarded. When the stack is full the oldest
   * snapshot is dropped.
   */
  apply(project: Project): void {
    this.states.length = this.cursor + 1;
    this.states.push(structuredClone(project));
    const cap = this.keep();
    if (this.states.length > cap) this.states.splice(0, this.states.length - cap);
    this.cursor = this.states.length - 1;
  }

  /**
   * Replace the CURRENT snapshot in place: an ongoing gesture's newer state,
   * not a new edit. The cursor, the number of snapshots and the capacity stay
   * as they are.
   */
  replace(project: Project): void {
    this.states[this.cursor] = structuredClone(project);
  }

  /** The snapshot one edit back, made current, or `null` at the oldest state. */
  undo(): Project | null {
    if (this.cursor === 0) return null;
    this.cursor -= 1;
    return structuredClone(this.states[this.cursor]);
  }

  /** The snapshot one edit forward, made current, or `null` at the newest. */
  redo(): Project | null {
    if (this.cursor + 1 >= this.states.length) return null;
    this.cursor += 1;
    return structuredClone(this.states[this.cursor]);
  }

  /** Is there an older snapshot to undo to? */
  canUndo(): boolean {
    return this.cursor > 0;
  }

  /** Is there a newer snapshot to redo to? */
  canRedo(): boolean {
    return this.cursor + 1 < this.states.length;
  }

  /** The snapshot the cursor names, as a copy so callers cannot edit history. */
  current(): Project {
    return structuredClone(this.states[this.cursor]);
  }
}

function normalize(capacity: number): number {
  return Number.isFinite(capacity) ? Math.max(0, Math.floor(capacity)) : DEFAULT_CAPACITY;
}
