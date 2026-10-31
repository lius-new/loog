import {stageExtensions} from './lib/extensions.mjs';
if(!process.argv[2])throw Error('Supply destination');
stageExtensions({destination:process.argv[2]});
