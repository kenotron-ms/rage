'use strict';
// Increments dist/run-count.txt in the current working directory.
// Called from each fixture package build script: node ../../../counter.cjs
const fs = require('fs');
const file = 'dist/run-count.txt';
fs.mkdirSync('dist', { recursive: true });
const n = +(fs.existsSync(file) ? fs.readFileSync(file, 'utf8').trim() : '0') + 1;
fs.writeFileSync(file, String(n));
