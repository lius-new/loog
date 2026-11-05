import {ensureLocked, stageBundled} from './lib/lock.mjs';
if(!process.argv[2])throw Error('Supply destination');
await ensureLocked();
stageBundled({destination:process.argv[2]});
