// Download the extensions named by assets/registry/extensions.lock into
// .packaging/locked, checking each against the lock.
import {LOCKED_DIR, LOCK_FILE, fetchLocked, readLock} from './lib/lock.mjs';

const lock = readLock();
if (!lock) throw new Error(`${LOCK_FILE} is missing; see scripts/package/lib/lock.mjs`);
const ids = await fetchLocked({lock, destination: LOCKED_DIR});
console.log(`Fetched ${ids.length} locked extension(s): ${ids.join(', ')}`);
