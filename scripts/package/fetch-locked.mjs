// Download the extensions named by assets/registry/extensions.lock into
// .packaging/locked, checking each against the lock. Does nothing without a lock.
import {LOCKED_DIR, fetchLocked, readLock} from './lib/lock.mjs';

const lock = readLock();
if (!lock) {
  console.log('No extensions.lock; the extensions in assets/extensions are shipped as they are.');
} else {
  const ids = await fetchLocked({lock, destination: LOCKED_DIR});
  console.log(`Fetched ${ids.length} locked extension(s): ${ids.join(', ')}`);
}
