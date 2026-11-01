import {stageBundled} from './lib/lock.mjs';
if(!process.argv[2])throw Error('Supply destination');
stageBundled({destination:process.argv[2]});
